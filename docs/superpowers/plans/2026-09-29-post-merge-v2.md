# Post-merge verification v2 (explicit state machine) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single `process_post_merge` function with the explicit, replayable state machine of spec revision 2: one rerun before a failure counts, a current-base check before any revert, safe revert per merge strategy, idempotent push/PR, per-target notifications from fixed templates, bounded infra retries, guaranteed worktree cleanup.

**Architecture:** A new module `crates/provefab/src/post_merge.rs` holds the pure types and decisions (states, failure kinds, rerun outcome, revert plan, templates) and an `impl Pipeline` block with one handler per state. `store.rs` gains a compare-and-set `advance_post_merge`. `forge.rs::Git` gains small, exact git helpers. `record_merge` in `pipeline.rs` only creates rows; the scheduler advances rows on every tick.

**Tech Stack:** Rust 2024, tokio, sqlx (SQLite, checksummed migrations), `gh`/`git` CLIs wrapped in `forge.rs`, `cargo nextest`. Tests use real git repos in tempdirs (`testkit::fixture`) and `testkit::FakeHub`.

**Spec:** `docs/specs/2026-09-29-post-merge-verification-design.md` (revision 2). Read it before Task 1. Section numbers below (§5, §6...) refer to it.

## Global Constraints

- Work in `provefab/` (the core repo). Crate: `crates/provefab`. Run commands from `provefab/`.
- Exactly ONE migration file: `crates/provefab/migrations/0004_post_merge.sql`, rewritten in place (never published; `~/.provefab/provefab.db` was verified on 2026-09-29 to have applied only migrations 1 to 3). A second migration file is a STOP: ask the owner.
- No new port trait. `Git` stays a concrete struct; `Hub` gains no method. Adding fields to `testkit::FakeHub` is allowed (test support, not a port). A new trait or `Hub` method is a STOP.
- Constants (in `post_merge.rs`): `ATTRIBUTION_WAIT_SECS = 3600`, `INFRA_ERROR_LIMIT = 5`, `BASE_MOVE_LIMIT = 3`, `SUMMARY_MAX = 2000`. One rerun per failing command, never more.
- Stage names for `stage_runs`: `post-merge`, `post-merge-base`, `revert-check`.
- Worktree paths: `<home>/post-merge/<check_id>-{verify,verify-rerun,base,base-rerun,revert,revert-check,revert-check-rerun}`. No worktree outlives the handler that created it.
- Revert branch: `provefab/revert-<check_id>-<base_moves>`. Provefab never deletes a remote branch, never force-pushes, never merges a revert.
- GitHub receives only text built by `post_merge::render` / `post_merge::revert_pr_body`: no stderr, no command output, no git/gh error text, no local path.
- No em-dashes in any user-facing text (templates, docs, CLI output).
- Every commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks before claiming any task done: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`. Integration tests need `--features testkit` if nextest does not enable it by default: run `cargo nextest run --all-features` when in doubt.
- The spec says "fake `Git`"; `Git` is concrete and the budget forbids a new trait, so tests drive real git in tempdirs. This is stronger evidence (real `git revert`, real pushes to a bare origin). Recorded here as a deliberate deviation.

## Review Focus

1. **A notification target that fails forever** (issue locked, PR deleted): a person expects Provefab to stop retrying after a bound, not to error every tick forever. Test in Task 8: 5 consecutive PR-comment failures mark the PR target as given up and the tick returns `Ok`.
2. **Multibyte command output or names near the 2,000-char bound**: truncation must never split a UTF-8 character or panic. Test in Task 2 (`bounded` on a string of `é`).
3. **A later, innocent merge while the base is still broken by an earlier one**: only the culprit gets a revert PR; the innocent merge is blocked (its revert does not fix the checks), never reverted. Test in Task 7.
4. **Repo removed from config, or `post_merge_checks` emptied, while a check is in flight**: nothing runs, nothing is posted, rows stay as they are, no panic. Test in Task 5.
5. **Crash residue that would make a failing commit look green** (a leftover worktree whose files were edited): must never be reused. Test in Task 5 (residue with a passing README on a failing commit still yields `verification_failed`).

---

## File map

- Create `crates/provefab/src/post_merge.rs`: types (`CheckState`, `FailureKind`, `FailedCommand`, `Confirmed`, `RevertPlan`), pure decisions (`confirm`, `revert_plan`, `bounded`), templates (`marker`, `render`, `revert_pr_body`), and the `impl Pipeline` state handlers.
- Modify `crates/provefab/src/lib.rs`: `pub mod post_merge;`.
- Rewrite `crates/provefab/migrations/0004_post_merge.sql`.
- Modify `crates/provefab/src/store.rs`: row type, `NewPostMergeCheck`, `CheckPatch`, `NoticeTarget`, store functions, migration checksum.
- Modify `crates/provefab/src/forge.rs`: git helpers; drop `worktree_detached`, `prepared_revert`, old `revert`.
- Modify `crates/provefab/src/pipeline.rs`: remove old post-merge code, `record_merge` attribution, helper visibility, auto-merge path.
- Modify `crates/provefab/src/scheduler.rs`: advance post-merge rows before the hourly throttle.
- Modify `crates/provefab/src/testkit.rs`: `FakeHub` per-URL PR status, revert PR heads, PR comment failures.
- Modify `crates/provefab/src/commands.rs`: `status`, `log`, `stats`.
- Rewrite `crates/provefab/tests/post_merge.rs`. Modify `crates/provefab/tests/scheduler.rs`.
- Docs: `docs/guide/{configuration,usage,operations}.md`, `README.md`, `provefab.example.toml`.

---

### Task 0: Baseline branch (needs the owner's go)

The v1 implementation is uncommitted on `main`. Implementation must not mix with it silently.

- [ ] **Step 1:** Ask the owner: "Commit the current uncommitted v1 feature-1 work as one baseline commit on a new branch `feature/post-merge-v2`?" Do not proceed without a yes.
- [ ] **Step 2:** On yes:

```bash
git switch -c feature/post-merge-v2
git add -A crates docs/guide docs/specs docs/superpowers README.md provefab.example.toml Cargo.lock docs/handoff-2026-09-29-feature1-post-merge.md
git status --short   # must show nothing left except files you did not intend to add; stop and ask if anything unexpected remains
git commit -m "wip: post-merge verification v1 (baseline before v2 state machine)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 3:** `cargo nextest run --all-features` passes (baseline green before any change). If it does not, stop and report.

---

### Task 1: Types, migration and store

**Files:**
- Create: `crates/provefab/src/post_merge.rs` (types only in this task)
- Modify: `crates/provefab/src/lib.rs`, `crates/provefab/migrations/0004_post_merge.sql`, `crates/provefab/src/store.rs`, `crates/provefab/src/pipeline.rs`, `crates/provefab/src/commands.rs`
- Delete content of: `crates/provefab/tests/post_merge.rs` (rewritten from Task 4)

**Interfaces:**
- Produces (in `provefab::post_merge`): `CheckState` (`ALL`, `as_str`, `parse`, `is_terminal`), `FailureKind` (`ALL`, `as_str`, `parse`), `FailedCommand { command: String, exit: Option<i32>, timed_out: bool }`, `SUMMARY_MAX`, `bounded(&str) -> String`.
- Produces (in `provefab::store`): `PostMergeCheckRow`, `NewPostMergeCheck<'a>`, `CheckPatch`, `NoticeTarget`, and `Store::{ensure_post_merge_check, post_merge_check_by_id, post_merge_check, post_merge_checks, advance_post_merge, post_merge_infra_error, mark_post_merge_notified, post_merge_state_counts}`.

- [ ] **Step 1: Rewrite the migration**

`crates/provefab/migrations/0004_post_merge.sql`:

```sql
-- Post-merge verification (docs/specs/2026-09-29-post-merge-verification-design.md, section 10).
-- One row per merged commit; `state` follows the section 5 state machine.
-- `merge_sha` is the literal 'unknown' when GitHub never reported it.
CREATE TABLE post_merge_checks (
    id                INTEGER PRIMARY KEY,
    task_id           INTEGER NOT NULL REFERENCES tasks (id),
    merge_sha         TEXT NOT NULL,
    base              TEXT NOT NULL,
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
```

- [ ] **Step 2: Types module**

Create `crates/provefab/src/post_merge.rs`:

```rust
//! Post-merge verification and safe rollback
//! (docs/specs/2026-09-29-post-merge-verification-design.md, revision 2).

use serde::{Deserialize, Serialize};

pub const ATTRIBUTION_WAIT_SECS: i64 = 3600;
pub const INFRA_ERROR_LIMIT: i64 = 5;
pub const BASE_MOVE_LIMIT: i64 = 3;
pub const SUMMARY_MAX: usize = 2000;

/// Where a check stands (spec section 5). One transition per scheduler tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Queued,
    Verifying,
    VerificationFailed,
    PreparingRevert,
    RevertReady,
    Passed,
    Superseded,
    RevertOpen,
    Blocked,
}

impl CheckState {
    pub const ALL: [CheckState; 9] = [
        Self::Queued,
        Self::Verifying,
        Self::VerificationFailed,
        Self::PreparingRevert,
        Self::RevertReady,
        Self::Passed,
        Self::Superseded,
        Self::RevertOpen,
        Self::Blocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Verifying => "verifying",
            Self::VerificationFailed => "verification_failed",
            Self::PreparingRevert => "preparing_revert",
            Self::RevertReady => "revert_ready",
            Self::Passed => "passed",
            Self::Superseded => "superseded",
            Self::RevertOpen => "revert_open",
            Self::Blocked => "blocked",
        }
    }

    /// Unknown text is `None`: the store turns it into a corruption error,
    /// never a default state.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Superseded | Self::RevertOpen | Self::Blocked
        )
    }
}

/// Why a check needs a human, or why verification failed (spec section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    CheckFailed,
    InfraError,
    RevertConflict,
    RevertChecksFailed,
    BaseMoved,
    BaseDiverged,
    BranchConflict,
    UnsafeMergeStrategy,
    AttributionMissing,
    DirtyTree,
}

impl FailureKind {
    pub const ALL: [FailureKind; 10] = [
        Self::CheckFailed,
        Self::InfraError,
        Self::RevertConflict,
        Self::RevertChecksFailed,
        Self::BaseMoved,
        Self::BaseDiverged,
        Self::BranchConflict,
        Self::UnsafeMergeStrategy,
        Self::AttributionMissing,
        Self::DirtyTree,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckFailed => "check_failed",
            Self::InfraError => "infra_error",
            Self::RevertConflict => "revert_conflict",
            Self::RevertChecksFailed => "revert_checks_failed",
            Self::BaseMoved => "base_moved",
            Self::BaseDiverged => "base_diverged",
            Self::BranchConflict => "branch_conflict",
            Self::UnsafeMergeStrategy => "unsafe_merge_strategy",
            Self::AttributionMissing => "attribution_missing",
            Self::DirtyTree => "dirty_tree",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A command that failed twice. Only these fields may reach GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCommand {
    pub command: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
}

/// At most `SUMMARY_MAX` characters, cut on a character boundary.
pub fn bounded(s: &str) -> String {
    if s.chars().count() <= SUMMARY_MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SUMMARY_MAX - 3).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_kinds_round_trip_and_reject_unknown_text() {
        for s in CheckState::ALL {
            assert_eq!(CheckState::parse(s.as_str()), Some(s));
        }
        for k in FailureKind::ALL {
            assert_eq!(FailureKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(CheckState::parse("failed"), None);
        assert_eq!(FailureKind::parse(""), None);
        let terminal: Vec<_> = CheckState::ALL.into_iter().filter(|s| s.is_terminal()).collect();
        assert_eq!(
            terminal,
            [CheckState::Passed, CheckState::Superseded, CheckState::RevertOpen, CheckState::Blocked]
        );
    }
}
```

Add `pub mod post_merge;` to `crates/provefab/src/lib.rs` (alphabetical, after `pub mod policy;`... keep the file's existing ordering: place it after `pub mod plugins;` / `pub mod policy;` so the list stays sorted).

- [ ] **Step 3: Store types and functions**

In `crates/provefab/src/store.rs`, replace the v1 `PostMergeCheckRow` and every v1 post-merge function (`ensure_post_merge_check`, `post_merge_check`, `post_merge_checks`, `set_post_merge_running`, `finish_post_merge`, `set_post_merge_revert_pr`, `mark_post_merge_notified`, `post_merge_check_row`) with:

```rust
use crate::post_merge::{CheckState, FailedCommand, FailureKind, bounded};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostMergeCheckRow {
    pub id: i64,
    pub task_id: i64,
    pub merge_sha: String,
    pub base: String,
    pub commit_count: Option<i64>,
    pub auto_merged: bool,
    pub state: CheckState,
    pub failure_kind: Option<FailureKind>,
    pub failure_summary: Option<String>,
    pub failed_commands: Vec<FailedCommand>,
    pub flaky: Vec<String>,
    pub base_sha: Option<String>,
    pub revert_sha: Option<String>,
    pub revert_branch: Option<String>,
    pub revert_pr_url: Option<String>,
    pub base_moves: i64,
    pub infra_errors: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub issue_notified_at: Option<i64>,
    pub pr_notified_at: Option<i64>,
}

pub struct NewPostMergeCheck<'a> {
    pub task_id: i64,
    pub merge_sha: &'a str,
    pub base: &'a str,
    pub commit_count: Option<usize>,
    pub auto_merged: bool,
}

/// Columns a transition sets. `None` keeps the stored value.
#[derive(Debug, Default, Clone)]
pub struct CheckPatch {
    pub failure_kind: Option<FailureKind>,
    pub failure_summary: Option<String>,
    pub failed_commands: Option<Vec<FailedCommand>>,
    pub flaky: Option<Vec<String>>,
    pub base_sha: Option<String>,
    pub revert_sha: Option<String>,
    pub revert_branch: Option<String>,
    pub revert_pr_url: Option<String>,
    pub bump_base_moves: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTarget {
    Issue,
    Pr,
}
```

Functions inside `impl Store`:

```rust
    /// Creates the one check for a merged commit, or returns the existing row
    /// when a scheduler retry sees the merge again.
    pub async fn ensure_post_merge_check(
        &self,
        new: &NewPostMergeCheck<'_>,
    ) -> Result<PostMergeCheckRow, StoreError> {
        sqlx::query(
            "INSERT INTO post_merge_checks (task_id, merge_sha, base, commit_count, auto_merged, state) \
             VALUES (?, ?, ?, ?, ?, 'queued') ON CONFLICT (task_id, merge_sha) DO NOTHING",
        )
        .bind(new.task_id)
        .bind(new.merge_sha)
        .bind(new.base)
        .bind(new.commit_count.map(|n| n as i64))
        .bind(new.auto_merged)
        .execute(&self.pool)
        .await?;
        self.post_merge_check(new.task_id, new.merge_sha)
            .await?
            .ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "missing post-merge check {}/{}",
                    new.task_id, new.merge_sha
                ))
            })
    }

    pub async fn post_merge_check_by_id(
        &self,
        id: i64,
    ) -> Result<Option<PostMergeCheckRow>, StoreError> {
        let row = sqlx::query("SELECT * FROM post_merge_checks WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| post_merge_check_row(&r)).transpose()
    }

    pub async fn post_merge_check(
        &self,
        task_id: i64,
        merge_sha: &str,
    ) -> Result<Option<PostMergeCheckRow>, StoreError> {
        let row =
            sqlx::query("SELECT * FROM post_merge_checks WHERE task_id = ? AND merge_sha = ?")
                .bind(task_id)
                .bind(merge_sha)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| post_merge_check_row(&r)).transpose()
    }

    pub async fn post_merge_checks(
        &self,
        task_id: i64,
    ) -> Result<Vec<PostMergeCheckRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM post_merge_checks WHERE task_id = ? ORDER BY id")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(post_merge_check_row).collect()
    }

    /// Compare-and-set transition (spec section 5): `false`, and nothing
    /// written, when the row is no longer in `from`. A successful transition
    /// resets the infra error count; a terminal one stamps `finished_at`.
    pub async fn advance_post_merge(
        &self,
        id: i64,
        from: CheckState,
        to: CheckState,
        patch: &CheckPatch,
    ) -> Result<bool, StoreError> {
        let json = |v: &Option<Vec<_>>| -> Result<Option<String>, StoreError> {
            v.as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::Corrupt(e.to_string()))
        };
        let flaky = patch
            .flaky
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let at = now();
        let done = sqlx::query(
            "UPDATE post_merge_checks SET state = ?, \
               failure_kind = COALESCE(?, failure_kind), \
               failure_summary = COALESCE(?, failure_summary), \
               failed_commands = COALESCE(?, failed_commands), \
               flaky = COALESCE(?, flaky), \
               base_sha = COALESCE(?, base_sha), \
               revert_sha = COALESCE(?, revert_sha), \
               revert_branch = COALESCE(?, revert_branch), \
               revert_pr_url = COALESCE(?, revert_pr_url), \
               base_moves = base_moves + ?, \
               infra_errors = 0, \
               started_at = COALESCE(started_at, ?), \
               finished_at = CASE WHEN ? THEN ? ELSE finished_at END \
             WHERE id = ? AND state = ?",
        )
        .bind(to.as_str())
        .bind(patch.failure_kind.map(FailureKind::as_str))
        .bind(patch.failure_summary.as_deref().map(bounded))
        .bind(json(&patch.failed_commands)?)
        .bind(flaky)
        .bind(patch.base_sha.as_deref())
        .bind(patch.revert_sha.as_deref())
        .bind(patch.revert_branch.as_deref())
        .bind(patch.revert_pr_url.as_deref())
        .bind(i64::from(patch.bump_base_moves))
        .bind(at)
        .bind(to.is_terminal())
        .bind(at)
        .bind(id)
        .bind(from.as_str())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Counts one more consecutive infra error; returns the new count.
    pub async fn post_merge_infra_error(&self, id: i64) -> Result<i64, StoreError> {
        let n: i64 = sqlx::query_scalar(
            "UPDATE post_merge_checks SET infra_errors = infra_errors + 1 WHERE id = ? RETURNING infra_errors",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    pub async fn mark_post_merge_notified(
        &self,
        id: i64,
        target: NoticeTarget,
    ) -> Result<(), StoreError> {
        let sql = match target {
            NoticeTarget::Issue => "UPDATE post_merge_checks SET issue_notified_at = ? WHERE id = ?",
            NoticeTarget::Pr => "UPDATE post_merge_checks SET pr_notified_at = ? WHERE id = ?",
        };
        sqlx::query(sql).bind(now()).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// `(state, count)` over every check, in `CheckState::ALL` order, zeros omitted.
    pub async fn post_merge_state_counts(&self) -> Result<Vec<(CheckState, i64)>, StoreError> {
        let rows = sqlx::query("SELECT state, COUNT(*) AS n FROM post_merge_checks GROUP BY state")
            .fetch_all(&self.pool)
            .await?;
        let mut counts = Vec::new();
        for s in CheckState::ALL {
            if let Some(r) = rows.iter().find(|r| r.get::<String, _>("state") == s.as_str()) {
                counts.push((s, r.get::<i64, _>("n")));
            }
        }
        Ok(counts)
    }
```

Row parser (next to `task_row`):

```rust
fn post_merge_check_row(r: &SqliteRow) -> Result<PostMergeCheckRow, StoreError> {
    let state: String = r.get("state");
    let kind: Option<String> = r.get("failure_kind");
    let json_vec = |col: &str| -> Result<Option<String>, StoreError> { Ok(r.get::<Option<String>, _>(col)) };
    let failed_commands = match json_vec("failed_commands")? {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        None => Vec::new(),
    };
    let flaky = match json_vec("flaky")? {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        None => Vec::new(),
    };
    Ok(PostMergeCheckRow {
        id: r.get("id"),
        task_id: r.get("task_id"),
        merge_sha: r.get("merge_sha"),
        base: r.get("base"),
        commit_count: r.get("commit_count"),
        auto_merged: r.get::<i64, _>("auto_merged") != 0,
        state: CheckState::parse(&state).ok_or_else(|| StoreError::Corrupt(state.clone()))?,
        failure_kind: kind
            .map(|k| FailureKind::parse(&k).ok_or(StoreError::Corrupt(k)))
            .transpose()?,
        failure_summary: r.get("failure_summary"),
        failed_commands,
        flaky,
        base_sha: r.get("base_sha"),
        revert_sha: r.get("revert_sha"),
        revert_branch: r.get("revert_branch"),
        revert_pr_url: r.get("revert_pr_url"),
        base_moves: r.get("base_moves"),
        infra_errors: r.get("infra_errors"),
        started_at: r.get("started_at"),
        finished_at: r.get("finished_at"),
        issue_notified_at: r.get("issue_notified_at"),
        pr_notified_at: r.get("pr_notified_at"),
    })
}
```

If `StoreError::Corrupt` takes a `String`, the calls above match; if `sqlx::query_scalar` with `RETURNING` is rejected by the SQLite version bundled, replace it with an `UPDATE` followed by `SELECT infra_errors FROM post_merge_checks WHERE id = ?`.

- [ ] **Step 4: Store tests (write before compiling the callers)**

Add to `mod tests` in `store.rs`:

```rust
    async fn merged_task(s: &Store) -> i64 {
        s.add_issue(&issue(1)).await.unwrap().unwrap()
    }

    fn new_check(task_id: i64, sha: &str) -> crate::store::NewPostMergeCheck<'_> {
        crate::store::NewPostMergeCheck {
            task_id,
            merge_sha: sha,
            base: "main",
            commit_count: Some(1),
            auto_merged: false,
        }
    }

    #[tokio::test]
    async fn post_merge_check_creation_is_idempotent() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let a = s.ensure_post_merge_check(&new_check(id, "abc")).await.unwrap();
        let b = s.ensure_post_merge_check(&new_check(id, "abc")).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.state, CheckState::Queued);
        assert_eq!(a.commit_count, Some(1));
        assert_eq!(s.post_merge_checks(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn advance_is_compare_and_set_and_patches_only_given_columns() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s.ensure_post_merge_check(&new_check(id, "abc")).await.unwrap();
        // Wrong `from`: nothing written.
        assert!(!s
            .advance_post_merge(c.id, CheckState::Verifying, CheckState::Passed, &CheckPatch::default())
            .await
            .unwrap());
        assert_eq!(s.post_merge_infra_error(c.id).await.unwrap(), 1);
        let patch = CheckPatch {
            base_sha: Some("base1".into()),
            failure_summary: Some("é".repeat(3000)),
            bump_base_moves: true,
            ..Default::default()
        };
        assert!(s
            .advance_post_merge(c.id, CheckState::Queued, CheckState::VerificationFailed, &patch)
            .await
            .unwrap());
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert_eq!(c.state, CheckState::VerificationFailed);
        assert_eq!(c.base_sha.as_deref(), Some("base1"));
        assert_eq!(c.failure_summary.as_ref().unwrap().chars().count(), crate::post_merge::SUMMARY_MAX);
        assert_eq!((c.base_moves, c.infra_errors), (1, 0));
        assert!(c.started_at.is_some() && c.finished_at.is_none());
        // `None` keeps `base_sha`; a terminal state stamps `finished_at`.
        assert!(s
            .advance_post_merge(c.id, CheckState::VerificationFailed, CheckState::Blocked, &CheckPatch::default())
            .await
            .unwrap());
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert_eq!(c.base_sha.as_deref(), Some("base1"));
        assert!(c.finished_at.is_some());
    }

    #[tokio::test]
    async fn an_unknown_post_merge_state_is_corruption_not_a_default() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s.ensure_post_merge_check(&new_check(id, "abc")).await.unwrap();
        sqlx::query("UPDATE post_merge_checks SET state = 'failed' WHERE id = ?")
            .bind(c.id)
            .execute(&s.pool)
            .await
            .unwrap();
        assert!(matches!(s.post_merge_check_by_id(c.id).await, Err(StoreError::Corrupt(_))));
    }

    #[tokio::test]
    async fn notification_targets_are_tracked_separately() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s.ensure_post_merge_check(&new_check(id, "abc")).await.unwrap();
        s.mark_post_merge_notified(c.id, NoticeTarget::Issue).await.unwrap();
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert!(c.issue_notified_at.is_some() && c.pr_notified_at.is_none());
    }
```

(Use `s.pool` if the field is private to the module: the tests module is inside `store.rs`, so it is visible.)

- [ ] **Step 5: Update the frozen migration checksum**

Run: `cargo nextest run -p provefab migrations_are_frozen`
Expected: FAIL, printing the new fourth checksum. Replace the fourth string in `migrations_are_frozen` with the printed value. Rerun: PASS.

- [ ] **Step 6: Make callers compile against the new API (no behavior yet)**

In `pipeline.rs`:
- Delete `post_merge_failure`, the v1 `process_post_merge`, and `notify_post_merge`.
- In `record_merge`, replace the v1 block that called `ensure_post_merge_check(task.id, sha, base)` and `finish_post_merge(... "blocked" ...)` with:

```rust
        if let (Some(sha), Some(base)) = (merge_sha, base)
            && base == repo.base
            && !repo.post_merge_checks.is_empty()
        {
            self.store
                .ensure_post_merge_check(&crate::store::NewPostMergeCheck {
                    task_id: task.id,
                    merge_sha: sha,
                    base,
                    commit_count,
                    auto_merged: false,
                })
                .await?;
        }
```

(Task 4 replaces this block with the full attribution rules.)

In `post_merge.rs`, add a temporary driver so the scheduler still compiles (Task 5 replaces its body):

```rust
use crate::agents::StageRunner;
use crate::pipeline::{Pipeline, PipelineError};
use crate::ports::{Hub, Oracle};

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Advances every post-merge check of a task by at most one transition.
    /// Task 5 implements the state handlers.
    pub async fn process_post_merge(&self, _task_id: i64) -> Result<(), PipelineError> {
        Ok(())
    }
}
```

In `commands.rs`, replace `c.state` string uses with `c.state.as_str()` and the v1 match in `stats` with:

```rust
            match check.state {
                CheckState::Passed => r.post_merge_passed += 1,
                CheckState::RevertOpen => {
                    r.post_merge_failed += 1;
                    r.revert_prs += 1;
                }
                CheckState::Blocked => r.post_merge_failed += 1,
                _ => {}
            }
```

(Task 9 rewrites these outputs; this step only keeps the build green.) Add `use crate::post_merge::CheckState;`.

Replace `crates/provefab/tests/post_merge.rs` with only:

```rust
#![cfg(feature = "testkit")]
//! Post-merge verification (spec revision 2). Tests are added from Task 4.
```

- [ ] **Step 7: Run everything**

Run: `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: all pass (the v1 post-merge integration tests are gone; store and type tests pass).

- [ ] **Step 8: Commit**

```bash
git add crates/provefab/src/post_merge.rs crates/provefab/src/lib.rs crates/provefab/migrations/0004_post_merge.sql crates/provefab/src/store.rs crates/provefab/src/pipeline.rs crates/provefab/src/commands.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: explicit check states and compare-and-set store (spec rev 2, section 5 and 10)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Pure decisions and templates

**Files:**
- Modify: `crates/provefab/src/post_merge.rs`

**Interfaces:**
- Consumes: `FailedCommand`, `FailureKind`, `CheckState`, `bounded` (Task 1); `crate::gates::GateReport` (`results: Vec<GateResult>`, each with `command`, `exit: Option<i32>`, `passed`, `timed_out`); `crate::store::PostMergeCheckRow`.
- Produces: `Confirmed { failed: Vec<FailedCommand>, flaky: Vec<String>, dirty: bool }`, `confirm(first: &GateReport, rerun: Option<&GateReport>) -> Confirmed`, `RevertPlan { Plain, Mainline1 }` with `fn mainline(self) -> Option<u8>`, `revert_plan(parents: usize, commit_count: Option<i64>, auto_merged: bool) -> Option<RevertPlan>`, `failure_summary(&[FailedCommand]) -> String`, `marker(check_id: i64) -> String`, `render(row: &PostMergeCheckRow) -> Option<String>`, `revert_pr_body(original_pr: &str, issue_url: &str, row: &PostMergeCheckRow) -> String`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `post_merge.rs`:

```rust
    use crate::gates::{GateReport, GateResult, ProgressScore};

    fn result(command: &str, passed: bool, exit: Option<i32>, timed_out: bool) -> GateResult {
        GateResult {
            command: command.into(),
            exit,
            passed,
            timed_out,
            output_tail: "SENTINEL_SECRET_42 /Users/someone/.provefab".into(),
            failing_tests: 0,
            error_lines: 0,
        }
    }

    fn report(results: Vec<GateResult>) -> GateReport {
        GateReport { results, score: ProgressScore::default() }
    }

    #[test]
    fn a_rerun_rescue_is_flaky_and_a_repeat_failure_is_confirmed() {
        let first = report(vec![
            result("a", false, Some(1), false),
            result("b", true, Some(0), false),
            result("c", false, None, true),
        ]);
        let rerun = report(vec![result("a", true, Some(0), false), result("c", false, None, true)]);
        let c = confirm(&first, Some(&rerun));
        assert_eq!(c.flaky, ["a"]);
        assert_eq!(
            c.failed,
            [FailedCommand { command: "c".into(), exit: None, timed_out: true }]
        );
        let clean = confirm(&report(vec![result("b", true, Some(0), false)]), None);
        assert!(clean.failed.is_empty() && clean.flaky.is_empty());
    }

    #[test]
    fn revert_plans_follow_the_merge_shape() {
        assert_eq!(revert_plan(2, Some(5), false), Some(RevertPlan::Mainline1));
        assert_eq!(revert_plan(1, Some(1), false), Some(RevertPlan::Plain));
        assert_eq!(revert_plan(1, Some(4), true), Some(RevertPlan::Plain));
        assert_eq!(revert_plan(1, Some(4), false), None);
        assert_eq!(revert_plan(1, None, true), None);
        assert_eq!(revert_plan(3, Some(1), false), None);
        assert_eq!(revert_plan(0, Some(1), false), None);
        assert_eq!(RevertPlan::Mainline1.mainline(), Some(1));
        assert_eq!(RevertPlan::Plain.mainline(), None);
    }

    #[test]
    fn bounded_never_splits_a_character() {
        let s = "é".repeat(SUMMARY_MAX + 10);
        let b = bounded(&s);
        assert_eq!(b.chars().count(), SUMMARY_MAX);
        assert!(b.ends_with("..."));
        assert_eq!(bounded("short"), "short");
    }

    fn row(state: CheckState, kind: Option<FailureKind>) -> crate::store::PostMergeCheckRow {
        crate::store::PostMergeCheckRow {
            id: 42,
            task_id: 7,
            merge_sha: "abc123".into(),
            base: "main".into(),
            commit_count: Some(1),
            auto_merged: false,
            state,
            failure_kind: kind,
            failure_summary: Some("SENTINEL_SECRET_42 at /Users/someone/.provefab/post-merge".into()),
            failed_commands: vec![FailedCommand { command: "cargo test".into(), exit: Some(101), timed_out: false }],
            flaky: vec![],
            base_sha: Some("def456".into()),
            revert_sha: Some("fed789".into()),
            revert_branch: Some("provefab/revert-42-0".into()),
            revert_pr_url: Some("https://github.com/o/r/pull/100".into()),
            base_moves: 0,
            infra_errors: 0,
            started_at: None,
            finished_at: None,
            issue_notified_at: None,
            pr_notified_at: None,
        }
    }

    #[test]
    fn every_published_text_is_a_fixed_template_with_its_marker() {
        let mut outcomes = vec![
            row(CheckState::Superseded, Some(FailureKind::CheckFailed)),
            row(CheckState::RevertOpen, Some(FailureKind::CheckFailed)),
        ];
        for k in FailureKind::ALL {
            outcomes.push(row(CheckState::Blocked, Some(k)));
        }
        for r in &outcomes {
            let body = render(r).expect("terminal failure outcomes are published");
            assert!(body.ends_with(&marker(42)), "{body}");
            assert!(!body.contains("SENTINEL_SECRET_42"), "{body}");
            assert!(!body.contains("/Users/"), "{body}");
            assert!(!body.contains('\u{2014}'), "no em-dash: {body}");
            assert!(body.contains("abc123"), "{body}");
        }
        assert!(render(&row(CheckState::RevertOpen, None)).unwrap().contains("https://github.com/o/r/pull/100"));
        assert!(render(&row(CheckState::Blocked, Some(FailureKind::RevertConflict))).unwrap().contains("`cargo test` exited with 101"));
        assert_eq!(render(&row(CheckState::Passed, None)), None);
        assert_eq!(render(&row(CheckState::Verifying, None)), None);
    }

    #[test]
    fn the_revert_pr_body_names_both_commits_and_never_auto_merges() {
        let body = revert_pr_body(
            "https://github.com/o/r/pull/8",
            "https://github.com/o/r/issues/7",
            &row(CheckState::RevertReady, Some(FailureKind::CheckFailed)),
        );
        for needle in [
            "https://github.com/o/r/pull/8",
            "https://github.com/o/r/issues/7",
            "abc123",
            "def456",
            "`cargo test` exited with 101",
            "Provefab will not merge this revert automatically.",
        ] {
            assert!(body.contains(needle), "{needle} missing from {body}");
        }
        assert!(!body.contains("SENTINEL_SECRET_42") && !body.contains('\u{2014}'));
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p provefab post_merge::tests`
Expected: compile errors (`confirm`, `revert_plan`, `render`... not found).

- [ ] **Step 3: Implement**

Add to `post_merge.rs` (above `mod tests`):

```rust
use crate::gates::GateReport;
use crate::store::PostMergeCheckRow;

/// Commands after at most one rerun (spec section 5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmed {
    pub failed: Vec<FailedCommand>,
    pub flaky: Vec<String>,
    /// A command modified tracked files: its result does not describe the commit.
    pub dirty: bool,
}

/// `rerun` holds only the commands that failed in `first`, run again on a
/// fresh checkout. A command that passes on the rerun is flaky, not failed.
pub fn confirm(first: &GateReport, rerun: Option<&GateReport>) -> Confirmed {
    let mut out = Confirmed::default();
    for r in first.results.iter().filter(|r| !r.passed) {
        let again = rerun.and_then(|rr| rr.results.iter().find(|x| x.command == r.command));
        match again {
            Some(x) if x.passed => out.flaky.push(r.command.clone()),
            Some(x) => out.failed.push(FailedCommand {
                command: x.command.clone(),
                exit: x.exit,
                timed_out: x.timed_out,
            }),
            None => out.failed.push(FailedCommand {
                command: r.command.clone(),
                exit: r.exit,
                timed_out: r.timed_out,
            }),
        }
    }
    out
}

/// How to undo a merge as one commit (spec section 6), or `None` when that
/// cannot be done safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertPlan {
    Plain,
    Mainline1,
}

impl RevertPlan {
    pub fn mainline(self) -> Option<u8> {
        match self {
            Self::Plain => None,
            Self::Mainline1 => Some(1),
        }
    }
}

pub fn revert_plan(parents: usize, commit_count: Option<i64>, auto_merged: bool) -> Option<RevertPlan> {
    match (parents, commit_count) {
        (2, _) => Some(RevertPlan::Mainline1),
        (1, Some(1)) => Some(RevertPlan::Plain),
        // Provefab's own merges are always squashes: one commit holds the whole PR.
        (1, Some(n)) if n > 1 && auto_merged => Some(RevertPlan::Plain),
        _ => None,
    }
}

/// Local summary of confirmed failures (bounded by the store).
pub fn failure_summary(failed: &[FailedCommand]) -> String {
    command_lines(failed)
}

fn command_lines(failed: &[FailedCommand]) -> String {
    failed
        .iter()
        .map(|f| {
            let how = if f.timed_out {
                "timed out".to_string()
            } else {
                match f.exit {
                    Some(code) => format!("exited with {code}"),
                    None => "was killed".to_string(),
                }
            };
            format!("- `{}` {how}", f.command)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn marker(check_id: i64) -> String {
    format!("<!-- provefab-post-merge:{check_id} -->")
}

fn blocked_reason(kind: Option<FailureKind>) -> &'static str {
    match kind {
        Some(FailureKind::InfraError) => {
            "Provefab could not finish after repeated infrastructure errors. Details are in the local provefab log."
        }
        Some(FailureKind::RevertConflict) => {
            "The checks failed, and reverting the merge does not apply cleanly on the current base."
        }
        Some(FailureKind::RevertChecksFailed) => {
            "The checks failed, and the proposed revert does not pass them either."
        }
        Some(FailureKind::BaseMoved) => {
            "The base branch kept moving while Provefab prepared the revert."
        }
        Some(FailureKind::BaseDiverged) => {
            "The merged commit is no longer an ancestor of the base branch."
        }
        Some(FailureKind::BranchConflict) => {
            "The revert branch or pull request on GitHub does not hold the revert Provefab prepared."
        }
        Some(FailureKind::UnsafeMergeStrategy) => {
            "This merge cannot be undone as one commit (a multi-commit pull request merged by rebase, or an unknown merge shape), so Provefab did not verify it."
        }
        Some(FailureKind::AttributionMissing) => {
            "GitHub did not report the merge commit or its base branch, so Provefab cannot tell what to verify."
        }
        Some(FailureKind::DirtyTree) => {
            "A check modified tracked files, so its result does not describe the committed tree."
        }
        Some(FailureKind::CheckFailed) | None => "The checks failed and no safe revert could be prepared.",
    }
}

/// The comment for a finished check, or `None` when nothing is published
/// (a pass, or a state that is not terminal). Fixed templates only (spec section 7).
pub fn render(row: &PostMergeCheckRow) -> Option<String> {
    let sha = &row.merge_sha;
    let failed = if row.failed_commands.is_empty() {
        String::new()
    } else {
        format!("\n\nFailed checks:\n{}", command_lines(&row.failed_commands))
    };
    let text = match row.state {
        CheckState::Superseded => format!(
            "Provefab post-merge checks failed on the merged commit `{sha}`.{failed}\n\nThe current `{}` tip `{}` passes the same checks, so no revert is proposed.",
            row.base,
            row.base_sha.as_deref().unwrap_or("unknown")
        ),
        CheckState::RevertOpen => format!(
            "Provefab post-merge checks failed on the merged commit `{sha}`.{failed}\n\nA revert pull request is open for human review: {}\nProvefab will not merge it automatically.",
            row.revert_pr_url.as_deref().unwrap_or("unknown")
        ),
        CheckState::Blocked => format!(
            "Provefab post-merge verification of `{sha}` needs a human. {}{failed}",
            blocked_reason(row.failure_kind)
        ),
        _ => return None,
    };
    Some(format!("{text}\n\n{}", marker(row.id)))
}

pub fn revert_pr_body(original_pr: &str, issue_url: &str, row: &PostMergeCheckRow) -> String {
    format!(
        "## Provefab post-merge verification failed\n\nOriginal pull request: {original_pr}\nIssue: {issue_url}\nMerged commit: `{}`\nBase tip: `{}`\n\nThese checks failed on the merged commit and still fail on the current base:\n\n{}\n\nThe reverted tree passes the same checks.\n\nProvefab will not merge this revert automatically.\n",
        row.merge_sha,
        row.base_sha.as_deref().unwrap_or("unknown"),
        command_lines(&row.failed_commands)
    )
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p provefab post_merge::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/post_merge.rs
git commit -m "post-merge: rerun confirmation, revert plan per merge shape, fixed GitHub templates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Git helpers and FakeHub support

**Files:**
- Modify: `crates/provefab/src/forge.rs`, `crates/provefab/src/testkit.rs`
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Produces on `Git`: `worktree_fresh_detached(repo, path, commit)`, `worktree_discard(repo, path)`, `parent_count(repo, commit) -> usize`, `revert(worktree, commit, mainline: Option<u8>)`, `branch_force(repo, name, commit)`, `branch_delete(repo, name)`, `remote_branch_sha(repo, branch) -> Option<String>`, `push_sha(repo, commit, branch)`, `clean(worktree) -> bool` (kept from v1). Removed: `worktree_detached`, `prepared_revert`.
- Produces on `FakeHub`: `revert_origin: Mutex<Option<PathBuf>>`, `pr_statuses: Mutex<HashMap<String, PrStatus>>`, `pr_head_override: Mutex<Option<String>>`, `pr_comment_failures: AtomicU32`.

- [ ] **Step 1: Write the failing tests**

Replace `crates/provefab/tests/post_merge.rs` with:

```rust
#![cfg(feature = "testkit")]
//! Post-merge verification (spec revision 2), against real git repos.

use std::path::Path;

use provefab::testkit::{fixture, git};

fn commit_and_push(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-qm", msg]);
    git(repo, &["push", "-q", "origin", "main"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn git_helpers_revert_exactly_and_never_reuse_residue() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git::default();
    let sha = commit_and_push(&repo, "README.md", "broken\n", "change");
    assert_eq!(g.parent_count(&repo, &sha).await.unwrap(), 1);

    // Residue at the path is discarded, never reused.
    let wt = f.home.join("post-merge").join("1-verify");
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    std::fs::write(wt.join("README.md"), "residue\n").unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap(), "broken\n");
    assert!(g.clean(&wt).await.unwrap());

    // A plain directory (not a registered worktree) is discarded too.
    g.worktree_discard(&repo, &wt).await.unwrap();
    std::fs::create_dir_all(wt.join("junk")).unwrap();
    g.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    assert!(!wt.join("junk").exists());

    g.revert(&wt, &sha, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap(), "hello\n");
    let revert = g.head(&wt).await.unwrap();

    assert_eq!(g.remote_branch_sha(&repo, "provefab/revert-1-0").await.unwrap(), None);
    g.branch_force(&repo, "provefab/revert-1-0", &revert).await.unwrap();
    g.push_sha(&repo, &revert, "provefab/revert-1-0").await.unwrap();
    assert_eq!(
        g.remote_branch_sha(&repo, "provefab/revert-1-0").await.unwrap().as_deref(),
        Some(revert.as_str())
    );
    g.worktree_discard(&repo, &wt).await.unwrap();
    assert!(!wt.exists());
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap();
    g.branch_delete(&repo, "provefab/revert-1-0").await.unwrap(); // missing is fine
}

#[tokio::test]
async fn a_merge_commit_reverts_with_mainline_one() {
    let f = fixture(&["true"]);
    let repo = f.config.repos[0].path_in(&f.home);
    let g = provefab::forge::Git::default();
    git(&repo, &["switch", "-qc", "feature"]);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["commit", "-qam", "feature"]);
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(g.parent_count(&repo, &merge).await.unwrap(), 2);
    let wt = f.home.join("post-merge").join("2-revert");
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    assert!(g.revert(&wt, &merge, None).await.is_err());
    g.worktree_fresh_detached(&repo, &wt, &merge).await.unwrap();
    g.revert(&wt, &merge, Some(1)).await.unwrap();
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap(), "hello\n");
}
```

If `Git::default()` does not exist, construct it the way `testkit::pipeline` does (read `testkit.rs` for the `git:` field initializer) and use that expression.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge`
Expected: compile errors for the missing methods.

- [ ] **Step 3: Implement the git helpers**

In `forge.rs`, delete `worktree_detached`, `prepared_revert`, and the v1 `revert`. Add inside `impl Git`:

```rust
    /// A fresh detached worktree at `commit`. Anything already at `path` (a
    /// crashed run's residue) is discarded first, never reused (spec section 9).
    pub async fn worktree_fresh_detached(
        &self,
        repo: &Path,
        path: &Path,
        commit: &str,
    ) -> Result<(), ForgeError> {
        self.worktree_discard(repo, path).await?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ForgeError::Parse(parent.display().to_string(), e.to_string()))?;
        }
        let dir = path.display().to_string();
        self.git(repo, &["worktree", "add", "--detach", &dir, commit])
            .await?;
        self.exclude_provefab_dir(path).await
    }

    /// Removes a worktree and its directory, registered or not. No-op when absent.
    pub async fn worktree_discard(&self, repo: &Path, path: &Path) -> Result<(), ForgeError> {
        if path.exists() {
            let dir = path.display().to_string();
            // Not a registered worktree (plain residue): git refuses; the
            // directory is removed below either way.
            let _ = self
                .git(repo, &["worktree", "remove", "--force", &dir])
                .await;
            if path.exists() {
                std::fs::remove_dir_all(path)
                    .map_err(|e| ForgeError::Parse(dir.clone(), e.to_string()))?;
            }
        }
        self.git(repo, &["worktree", "prune"]).await?;
        Ok(())
    }

    /// 1 for an ordinary or squash commit, 2 for a merge commit.
    pub async fn parent_count(&self, repo: &Path, commit: &str) -> Result<usize, ForgeError> {
        let line = self
            .git(repo, &["rev-list", "--parents", "-n", "1", commit])
            .await?;
        Ok(line.split_whitespace().count().saturating_sub(1))
    }

    /// Reverts one commit with hooks off; `mainline` is `Some(1)` for a merge
    /// commit. A conflict is returned as the git error, never resolved.
    pub async fn revert(
        &self,
        worktree: &Path,
        commit: &str,
        mainline: Option<u8>,
    ) -> Result<(), ForgeError> {
        let m = mainline.map(|m| m.to_string());
        let mut args = vec!["revert", "--no-edit"];
        if let Some(m) = m.as_deref() {
            args.extend(["-m", m]);
        }
        args.push(commit);
        self.git(worktree, &args).await?;
        Ok(())
    }

    /// Points a local branch at `commit` (keeps the revert commit reachable).
    pub async fn branch_force(&self, repo: &Path, name: &str, commit: &str) -> Result<(), ForgeError> {
        self.git(repo, &["branch", "-f", name, commit]).await?;
        Ok(())
    }

    /// Deletes a local branch; a missing branch is not an error.
    pub async fn branch_delete(&self, repo: &Path, name: &str) -> Result<(), ForgeError> {
        match self.git(repo, &["branch", "-D", name]).await {
            Ok(_) | Err(ForgeError::Failed { code: Some(1), .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The commit `origin` holds for `branch`, if the branch exists there.
    pub async fn remote_branch_sha(&self, repo: &Path, branch: &str) -> Result<Option<String>, ForgeError> {
        let out = self
            .git(repo, &["ls-remote", "origin", &format!("refs/heads/{branch}")])
            .await?;
        Ok(out.split_whitespace().next().map(str::to_string))
    }

    /// Pushes an exact commit to a branch, never forcing.
    pub async fn push_sha(&self, repo: &Path, commit: &str, branch: &str) -> Result<(), ForgeError> {
        self.git(
            repo,
            &["push", "--no-verify", "origin", &format!("{commit}:refs/heads/{branch}")],
        )
        .await?;
        Ok(())
    }
```

Keep the v1 `clean` method as is. Check `git branch -D` exit code on a missing branch on this machine (`git branch -D nope; echo $?`): if it is not 1, match the observed code instead and note it in the doc comment.

- [ ] **Step 4: FakeHub support for revert PRs**

In `testkit.rs`, add fields to `FakeHub` (and initialize them in `FakeHub::new`):

```rust
    /// When set, `pr_create` behaves like GitHub for revert PRs: it reuses an
    /// open PR with the same head, answers a new URL per head, and records the
    /// head commit read from this bare origin.
    pub revert_origin: Mutex<Option<PathBuf>>,
    /// Per-URL answers of `pr_status` (falls back to `pr_status`).
    pub pr_statuses: Mutex<HashMap<String, crate::forge::PrStatus>>,
    /// Reports this head for the next PR `pr_create` returns (a reused PR on other work).
    pub pr_head_override: Mutex<Option<String>>,
    /// The next this-many `pr_comment` calls fail (GitHub unreachable).
    pub pr_comment_failures: std::sync::atomic::AtomicU32,
```

Initializers: `revert_origin: Mutex::new(None)`, `pr_statuses: Mutex::new(HashMap::new())`, `pr_head_override: Mutex::new(None)`, `pr_comment_failures: Default::default()`. Add `use std::collections::HashMap;` if missing.

In `impl Hub for FakeHub`:

`pr_status`:

```rust
    async fn pr_status(&self, _: &str, url: &str) -> Result<crate::forge::PrStatus, ForgeError> {
        if let Some(s) = self.pr_statuses.lock().unwrap().get(url) {
            return Ok(s.clone());
        }
        Ok(self.pr_status.lock().unwrap().clone())
    }
```

`pr_comment`:

```rust
    async fn pr_comment(&self, _: &str, url: &str, body: &str) -> Result<(), ForgeError> {
        use std::sync::atomic::Ordering;
        let left = self.pr_comment_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.pr_comment_failures.store(left - 1, Ordering::SeqCst);
            return Err(ForgeError::Parse("gh pr comment".into(), "HTTP 502".into()));
        }
        self.posted.lock().unwrap().push(body.to_string());
        let comment = Comment {
            author: "me".into(),
            association: "MEMBER".into(),
            body: crate::forge::with_prefix(body),
            created_at: crate::store::rfc3339(crate::store::now()),
        };
        match self.pr_statuses.lock().unwrap().get_mut(url) {
            Some(s) => s.comments.push(comment),
            None => self.pr_status.lock().unwrap().comments.push(comment),
        }
        Ok(())
    }
```

In `pr_create`, after the failure-injection block and before the existing `self.prs...push(...)`, insert:

```rust
        if let Some(origin) = self.revert_origin.lock().unwrap().clone() {
            let mut prs = self.prs.lock().unwrap();
            let url = match prs.iter().position(|p| p.0 == head && p.1 == base) {
                Some(i) => format!("https://github.com/o/r/pull/{}", 100 + i),
                None => {
                    prs.push((head.into(), base.into(), title.into(), body.into()));
                    format!("https://github.com/o/r/pull/{}", 100 + prs.len() - 1)
                }
            };
            let real = git(&origin, &["rev-parse", &format!("refs/heads/{head}")]);
            let head_sha = self.pr_head_override.lock().unwrap().take().unwrap_or(real);
            self.pr_statuses
                .lock()
                .unwrap()
                .entry(url.clone())
                .or_insert(crate::forge::PrStatus {
                    state: crate::forge::PrState::Open,
                    comments: Vec::new(),
                    head_sha: Some(head_sha),
                    merge_sha: None,
                    base_ref: Some(base.into()),
                    commit_count: Some(1),
                });
            return Ok(url);
        }
```

(`git` is the testkit helper defined in the same file.)

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run --all-features --test post_merge`
Expected: PASS. Then `cargo clippy --all-targets --all-features -- -D warnings`: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/forge.rs crates/provefab/src/testkit.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: exact git helpers (fresh worktrees, parent count, mainline revert, sha push) and FakeHub revert PRs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Merge attribution and scheduler cadence

**Files:**
- Modify: `crates/provefab/src/pipeline.rs` (`record_merge`, auto-merge path in `impl MergeTools for PipelineTools`)
- Modify: `crates/provefab/src/scheduler.rs`
- Test: `crates/provefab/tests/post_merge.rs`, `crates/provefab/tests/scheduler.rs`

**Interfaces:**
- Consumes: `NewPostMergeCheck`, `CheckPatch`, `advance_post_merge`, `ATTRIBUTION_WAIT_SECS`, `FailureKind::AttributionMissing` (Task 1).
- Produces: rows created by `watch_pr` per spec section 4; task output `merge_seen` `{"at": <unix secs>}` (first time a merged PR lacked attribution).

- [ ] **Step 1: Write the failing tests**

Append to `tests/post_merge.rs` (add these imports at the top: `use provefab::post_merge::{CheckState, FailureKind};`, `use provefab::task::TaskState;`, `use provefab::testkit::{FakeHub, FakeOracle, Fixture, Script, pipeline, queue};`, `use serde_json::json;`):

```rust
type P = provefab::pipeline::Pipeline<provefab::testkit::FakeRunner, FakeOracle, FakeHub>;

fn unused_worker(
    _: &provefab::config::ModelEntry,
    _: &agent_workers::StageRequest,
    _: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    None
}

/// A task whose Provefab PR (pull/8) is open, in a repo with `checks`.
async fn open_pr_task(checks: &[&str]) -> (Fixture, P, i64) {
    let mut f = fixture(&["true"]);
    f.config.repos[0].post_merge_checks = checks.iter().map(|c| c.to_string()).collect();
    let p = pipeline(&f, Box::new(unused_worker) as Box<Script>, FakeOracle::default(), FakeHub::new("issue")).await;
    *p.hub.revert_origin.lock().unwrap() = Some(f.origin.clone());
    let id = queue(&p).await;
    p.store.set_pr(id, "https://github.com/o/r/pull/8", "open").await.unwrap();
    p.store.transition(id, TaskState::PrOpen, "pr").await.unwrap();
    (f, p, id)
}

fn merged(sha: Option<&str>, base: Option<&str>, commits: Option<usize>) -> provefab::forge::PrStatus {
    provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("pr-head".into()),
        merge_sha: sha.map(Into::into),
        base_ref: base.map(Into::into),
        commit_count: commits,
    }
}

#[tokio::test]
async fn a_merge_on_the_configured_base_queues_one_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!((c[0].merge_sha.as_str(), c[0].state), ("abc", CheckState::Queued));
    assert_eq!(p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(), Some("merged"));
}

#[tokio::test]
async fn another_base_or_no_opt_in_creates_no_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("release"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());

    let (_f, p, id) = open_pr_task(&[]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_attribution_waits_an_hour_then_blocks() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(None, Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
    assert_eq!(p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(), Some("open"));
    // An hour later GitHub still has no merge commit: record the merge, block the check.
    let long_ago = provefab::store::now() - provefab::post_merge::ATTRIBUTION_WAIT_SECS - 1;
    p.store.record_output(id, "merge_seen", &json!({"at": long_ago})).await.unwrap();
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c[0].merge_sha, "unknown");
    assert_eq!((c[0].state, c[0].failure_kind), (CheckState::Blocked, Some(FailureKind::AttributionMissing)));
    assert_eq!(p.store.task(id).await.unwrap().unwrap().pr_state.as_deref(), Some("merged"));
}

#[tokio::test]
async fn a_missing_base_is_never_inferred() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), None, Some(1));
    let long_ago = provefab::store::now() - provefab::post_merge::ATTRIBUTION_WAIT_SECS - 1;
    p.store.record_output(id, "merge_seen", &json!({"at": long_ago})).await.unwrap();
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert_eq!(c[0].failure_kind, Some(FailureKind::AttributionMissing));
}

#[tokio::test]
async fn an_auto_merge_is_recorded_on_the_check() {
    let (_f, p, id) = open_pr_task(&["true"]).await;
    p.store.record_output(id, "auto_merged", &json!({"head": "pr-head"})).await.unwrap();
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(3));
    p.watch_pr(id).await.unwrap();
    let c = p.store.post_merge_checks(id).await.unwrap();
    assert!(c[0].auto_merged);
    assert_eq!(c[0].commit_count, Some(3));
}

#[tokio::test]
async fn opting_in_later_never_checks_an_old_merge() {
    let (_f, p, id) = open_pr_task(&[]).await;
    *p.hub.pr_status.lock().unwrap() = merged(Some("abc"), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    // The owner opts in after the merge was recorded.
    let mut p = p;
    p.config.repos[0].post_merge_checks = vec!["true".into()];
    p.watch_pr(id).await.unwrap();
    p.process_post_merge(id).await.unwrap();
    assert!(p.store.post_merge_checks(id).await.unwrap().is_empty());
}
```

Append to `tests/scheduler.rs`:

```rust
/// Post-merge rows advance on every tick, not on the hourly reopen watch.
#[tokio::test]
async fn post_merge_checks_advance_on_every_tick() {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = vec!["true".into()];
    let p = std::sync::Arc::new(
        pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await,
    );
    let source = FakeSource(p.hub.issue.clone());
    within(10, provefab::scheduler::run(p.clone(), &source, ONCE, std::future::pending::<()>()))
        .await
        .unwrap();
    // A real one-parent commit on origin/main stands in for the squash merge.
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("MERGED.md"), "merged\n").unwrap();
    git(&repo, &["add", "MERGED.md"]);
    git(&repo, &["commit", "-qm", "squash merge"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    for _ in 0..4 {
        within(10, provefab::scheduler::run(p.clone(), &source, ONCE, std::future::pending::<()>()))
            .await
            .unwrap();
    }
    let t = p.store.task_by_url("https://github.com/o/r/issues/7").await.unwrap().unwrap();
    let c = p.store.post_merge_checks(t.id).await.unwrap();
    assert_eq!(c[0].state, provefab::post_merge::CheckState::Passed);
}
```

(Tick 1 of the loop records the merge; ticks 2 and 3 move `queued` to `verifying` to `passed`. The 4th is slack.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge --test scheduler`
Expected: `missing_attribution_waits_an_hour_then_blocks`, `a_missing_base_is_never_inferred`, `an_auto_merge_is_recorded_on_the_check` fail; `post_merge_checks_advance_on_every_tick` fails (the driver is still a no-op; it passes only after Task 5, which is expected: keep it and recheck at Task 5).

- [ ] **Step 3: Implement `record_merge` attribution**

In `pipeline.rs`, at the start of `record_merge`, replace the v1 early return with:

```rust
        if !repo.post_merge_checks.is_empty() && (merge_sha.is_none() || base.is_none()) {
            // GitHub may report `mergeCommit` a little after the merge: keep the PR
            // watched, but never longer than an hour (spec section 4).
            let first = match self.store.last_output(task.id, "merge_seen").await? {
                Some(v) => v["at"].as_i64().unwrap_or_else(now),
                None => {
                    self.store
                        .record_output(task.id, "merge_seen", &json!({"at": now()}))
                        .await?;
                    now()
                }
            };
            if now() - first < crate::post_merge::ATTRIBUTION_WAIT_SECS {
                return Ok(());
            }
        }
```

Replace the Task 1 `if let (Some(sha), Some(base)) ...` block with:

```rust
        if !repo.post_merge_checks.is_empty() {
            let auto_merged = self.store.last_output(task.id, "auto_merged").await?.is_some();
            match (merge_sha, base) {
                (Some(sha), Some(b)) if b == repo.base => {
                    self.store
                        .ensure_post_merge_check(&crate::store::NewPostMergeCheck {
                            task_id: task.id,
                            merge_sha: sha,
                            base: b,
                            commit_count,
                            auto_merged,
                        })
                        .await?;
                }
                // Merged into another branch: not what the checks describe.
                (Some(_), Some(_)) => {}
                _ => {
                    let row = self
                        .store
                        .ensure_post_merge_check(&crate::store::NewPostMergeCheck {
                            task_id: task.id,
                            merge_sha: merge_sha.unwrap_or("unknown"),
                            base: base.unwrap_or(&repo.base),
                            commit_count,
                            auto_merged,
                        })
                        .await?;
                    self.store
                        .advance_post_merge(
                            row.id,
                            crate::post_merge::CheckState::Queued,
                            crate::post_merge::CheckState::Blocked,
                            &crate::store::CheckPatch {
                                failure_kind: Some(crate::post_merge::FailureKind::AttributionMissing),
                                failure_summary: Some(format!(
                                    "GitHub reported merge commit {merge_sha:?} and base {base:?} for over an hour"
                                )),
                                ..Default::default()
                            },
                        )
                        .await?;
                }
            }
        }
```

In the auto-merge path (`impl MergeTools for PipelineTools`, the `record_merge` call after `auto_merged`), replace `status.base_ref.as_deref().or(Some(self.repo.base.as_str()))` with `status.base_ref.as_deref()`. The `watch_pr` test above covers the same `record_merge` code this path calls.

- [ ] **Step 4: Scheduler cadence**

In `scheduler.rs`, inside the `PrOpen` arm, move the `process_post_merge` call from after `watch_pr` to before the `if matches!(t.pr_state.as_deref(), Some("merged" | "done"))` throttle block:

```rust
                        // Post-merge rows advance every tick; the hourly throttle
                        // below is for the reopen watch only (spec section 5).
                        if let Err(e) = p.process_post_merge(t.id).await {
                            eprintln!("provefab: post-merge verification for task {}: {e}", t.id);
                        }
```

and delete the old call after `watch_pr`.

- [ ] **Step 5: Run**

Run: `cargo nextest run --all-features --test post_merge`
Expected: all Task 4 tests in `post_merge.rs` PASS. `post_merge_checks_advance_on_every_tick` still fails until Task 5.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/pipeline.rs crates/provefab/src/scheduler.rs crates/provefab/tests/post_merge.rs crates/provefab/tests/scheduler.rs
git commit -m "post-merge: attribution from GitHub only (1 h wait, no inferred base), advance every tick

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Driver, `queued`, `verifying`, infra errors, cleanup

**Files:**
- Modify: `crates/provefab/src/post_merge.rs`, `crates/provefab/src/pipeline.rs` (visibility, `gates` stage list)
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Consumes: `pub(crate)` pipeline helpers `task`, `repo`, `checkout`, `repo_lock`, `gates`, `tell`; Git helpers (Task 3); `confirm`, `revert_plan`, `failure_summary` (Task 2); store API (Task 1).
- Produces (private to `post_merge.rs`, used by Tasks 6 to 8): `pm_dir(check_id, suffix) -> PathBuf`, `pm_run(task, repo, check, commit, suffix, stage, require_clean) -> Result<Confirmed, PipelineError>`, `advance(check, to, patch) -> Result<(), PipelineError>`, `block(check, kind, summary) -> Result<(), PipelineError>`, `step_post_merge(task, repo, check)`, `finish_terminal(repo, check)`, `notify_post_merge(task, repo, check)` (stub returning `Ok(())` here, implemented in Task 8).

- [ ] **Step 1: Write the failing tests**

Append to `tests/post_merge.rs`:

```rust
/// README.md becomes `change` in one commit merged on origin/main, recorded as
/// a merged Provefab PR (pull/8) with a queued check.
async fn setup(checks: &[&str], change: &str) -> (Fixture, P, i64, String) {
    let (f, p, id) = open_pr_task(checks).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha = commit_and_push(&repo, "README.md", change, "Merged Provefab change");
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    // Later comments on pull/8 go to its own per-URL status.
    let status = p.hub.pr_status.lock().unwrap().clone();
    p.hub.pr_statuses.lock().unwrap().insert("https://github.com/o/r/pull/8".into(), status);
    (f, p, id, sha)
}

async fn check(p: &P, id: i64) -> provefab::store::PostMergeCheckRow {
    p.store.post_merge_checks(id).await.unwrap().remove(0)
}

async fn tick(p: &P, id: i64) -> CheckState {
    p.process_post_merge(id).await.unwrap();
    check(p, id).await.state
}

/// Ticks until the check is terminal.
async fn drive(p: &P, id: i64) -> provefab::store::PostMergeCheckRow {
    for _ in 0..12 {
        if tick(p, id).await.is_terminal() {
            return check(p, id).await;
        }
    }
    panic!("not terminal after 12 ticks: {:?}", check(p, id).await.state);
}

fn leftovers(f: &Fixture, check_id: i64) -> Vec<String> {
    let dir = f.home.join("post-merge");
    std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&format!("{check_id}-")))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_green_merge_passes_in_two_ticks_and_posts_nothing() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "hello world\n").await;
    assert_eq!(tick(&p, id).await, CheckState::Verifying);
    assert_eq!(tick(&p, id).await, CheckState::Passed);
    let c = check(&p, id).await;
    assert!(c.flaky.is_empty() && c.finished_at.is_some());
    assert!(p.hub.posted.lock().unwrap().is_empty());
    assert!(leftovers(&f, c.id).is_empty());
    // A terminal check costs nothing more.
    assert_eq!(tick(&p, id).await, CheckState::Passed);
}

#[tokio::test]
async fn a_failure_rescued_by_its_rerun_is_flaky_not_failed() {
    let (f, p, id, _) = setup(&["true"], "hello\n").await;
    let mark = f.home.join("flaky-mark");
    let cmd = format!("test -f {0} || {{ touch {0}; false; }}", mark.display());
    let mut p = p;
    p.config.repos[0].post_merge_checks = vec![cmd.clone()];
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::Passed);
    assert_eq!(c.flaky, [cmd]);
}

#[tokio::test]
async fn a_failure_twice_is_confirmed_and_timeouts_count_as_failures() {
    let (_f, p, id, _) = setup(&["sleep 5"], "hello\n").await;
    let mut p = p;
    p.config.limits.gate_timeout = std::time::Duration::from_millis(300);
    tick(&p, id).await;
    assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
    let c = check(&p, id).await;
    assert_eq!(c.failure_kind, Some(FailureKind::CheckFailed));
    assert!(c.failed_commands[0].timed_out);
}

#[tokio::test]
async fn crash_residue_that_looks_green_is_never_reused() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    assert_eq!(tick(&p, id).await, CheckState::Verifying);
    let c = check(&p, id).await;
    // A crashed run left a worktree whose README would pass.
    let repo = f.config.repos[0].path_in(&f.home);
    let wt = f.home.join("post-merge").join(format!("{}-verify", c.id));
    p.git.worktree_fresh_detached(&repo, &wt, &sha).await.unwrap();
    std::fs::write(wt.join("README.md"), "hello\n").unwrap();
    assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_human_merged_multi_commit_pr_is_blocked_before_running_anything() {
    let (f, p, id) = open_pr_task(&["touch ran; true"]).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha = commit_and_push(&repo, "README.md", "x\n", "change");
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(3));
    p.watch_pr(id).await.unwrap();
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::UnsafeMergeStrategy)));
    assert!(p.store.stage_runs(id).await.unwrap().iter().all(|r| r.stage != "post-merge"));
}

#[tokio::test]
async fn a_removed_repo_or_emptied_checks_leave_rows_untouched() {
    let (_f, p, id, _) = setup(&["true"], "hello\n").await;
    let mut p = p;
    p.config.repos[0].post_merge_checks.clear();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(check(&p, id).await.state, CheckState::Queued);
    p.config.repos.clear();
    p.process_post_merge(id).await.unwrap();
    assert_eq!(check(&p, id).await.state, CheckState::Queued);
    assert!(p.hub.posted.lock().unwrap().is_empty());
}
```

If `Store::stage_runs(task_id)` has another name, use the store function that `commands::log` uses to list stage runs (read `commands.rs::log`).

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge`
Expected: the new tests FAIL (driver is a no-op: states stay `queued`).

- [ ] **Step 3: Pipeline helper visibility**

In `pipeline.rs`, change these to `pub(crate)`: `fn repo`, `async fn task`, `async fn tell`, `fn checkout`, `fn repo_lock`, `async fn gates`. In `gates`, change the results-file condition to:

```rust
        if matches!(stage, "post-merge" | "post-merge-base" | "revert-check") {
```

- [ ] **Step 4: Implement the driver and the first two states**

In `post_merge.rs`, replace the Task 1 stub `impl` block with (extend the `use` lines as needed: `std::path::PathBuf`, `crate::config::RepoConfig`, `crate::forge::ForgeError`, `crate::store::{CheckPatch, NoticeTarget, StoreError, TaskRow}`):

```rust
impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Advances every post-merge check of a task by at most one transition,
    /// then publishes pending notices (spec sections 5 and 7). Rows exist only
    /// for merges attributed by `record_merge`.
    pub async fn process_post_merge(&self, task_id: i64) -> Result<(), PipelineError> {
        let task = self.task(task_id).await?;
        let Some(repo) = self.repo(&task).cloned() else {
            return Ok(());
        };
        if repo.post_merge_checks.is_empty() {
            return Ok(());
        }
        let mut first_error = None;
        for check in self.store.post_merge_checks(task_id).await? {
            if !check.state.is_terminal() {
                let lock = self.repo_lock(&repo);
                let guard = lock.lock().await;
                let result = self.step_post_merge(&task, &repo, &check).await;
                // Guard of spec section 9: no worktree outlives its handler.
                self.discard_check_worktrees(&repo, check.id).await;
                drop(guard);
                if let Err(e) = result {
                    let n = self.store.post_merge_infra_error(check.id).await?;
                    if n >= INFRA_ERROR_LIMIT {
                        let now = self.reload(&check).await?;
                        self.block(&now, FailureKind::InfraError, &e.to_string()).await?;
                    } else {
                        eprintln!("provefab: post-merge check {} ({}): {e}", check.id, check.state.as_str());
                        first_error.get_or_insert(e);
                        continue;
                    }
                }
                let now = self.reload(&check).await?;
                if now.state.is_terminal() {
                    self.finish_terminal(&repo, &now).await;
                }
            }
            let now = self.reload(&check).await?;
            if now.state.is_terminal()
                && let Err(e) = self.notify_post_merge(&task, &repo, &now).await
            {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn reload(&self, check: &PostMergeCheckRow) -> Result<PostMergeCheckRow, PipelineError> {
        self.store
            .post_merge_check_by_id(check.id)
            .await?
            .ok_or_else(|| StoreError::Corrupt(format!("post-merge check {} vanished", check.id)).into())
    }

    async fn step_post_merge(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        match check.state {
            CheckState::Queued => self.pm_queued(repo, check).await,
            CheckState::Verifying => self.pm_verifying(task, repo, check).await,
            CheckState::VerificationFailed => self.pm_verification_failed(task, repo, check).await,
            CheckState::PreparingRevert => self.pm_preparing_revert(task, repo, check).await,
            CheckState::RevertReady => self.pm_revert_ready(task, repo, check).await,
            CheckState::Passed | CheckState::Superseded | CheckState::RevertOpen | CheckState::Blocked => Ok(()),
        }
    }

    fn pm_dir(&self, check_id: i64, suffix: &str) -> PathBuf {
        self.paths.home.join("post-merge").join(format!("{check_id}-{suffix}"))
    }

    const WORKTREE_SUFFIXES: [&'static str; 7] = [
        "verify",
        "verify-rerun",
        "base",
        "base-rerun",
        "revert",
        "revert-check",
        "revert-check-rerun",
    ];

    async fn discard_check_worktrees(&self, repo: &RepoConfig, check_id: i64) {
        let repo_path = self.checkout(repo);
        for suffix in Self::WORKTREE_SUFFIXES {
            let wt = self.pm_dir(check_id, suffix);
            if let Err(e) = self.git.worktree_discard(&repo_path, &wt).await {
                eprintln!("provefab: could not remove {}: {e}", wt.display());
            }
        }
    }

    /// Terminal cleanup: the local revert branch goes; remote branches never do.
    async fn finish_terminal(&self, repo: &RepoConfig, check: &PostMergeCheckRow) {
        if let Some(branch) = &check.revert_branch {
            let _ = self.git.branch_delete(&self.checkout(repo), branch).await;
        }
    }

    async fn advance(
        &self,
        check: &PostMergeCheckRow,
        to: CheckState,
        patch: CheckPatch,
    ) -> Result<(), PipelineError> {
        self.store.advance_post_merge(check.id, check.state, to, &patch).await?;
        Ok(())
    }

    async fn block(
        &self,
        check: &PostMergeCheckRow,
        kind: FailureKind,
        summary: &str,
    ) -> Result<(), PipelineError> {
        self.advance(
            check,
            CheckState::Blocked,
            CheckPatch {
                failure_kind: Some(kind),
                failure_summary: Some(summary.to_string()),
                ..Default::default()
            },
        )
        .await
    }

    /// Runs the checks on a fresh detached checkout of `commit`, then reruns
    /// each failure once on another fresh checkout (spec section 5).
    #[allow(clippy::too_many_arguments)]
    async fn pm_run(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
        commit: &str,
        suffix: &str,
        stage: &str,
        require_clean: bool,
    ) -> Result<Confirmed, PipelineError> {
        let repo_path = self.checkout(repo);
        let wt = self.pm_dir(check.id, suffix);
        self.git.worktree_fresh_detached(&repo_path, &wt, commit).await?;
        let first = self.gates(task, &wt, &repo.post_merge_checks, stage).await?;
        let dirty = require_clean && !self.git.clean(&wt).await?;
        self.git.worktree_discard(&repo_path, &wt).await?;
        if first.passed() || dirty {
            return Ok(Confirmed { dirty, ..confirm(&first, None) });
        }
        let failing: Vec<String> = first
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| r.command.clone())
            .collect();
        let rerun_wt = self.pm_dir(check.id, &format!("{suffix}-rerun"));
        self.git.worktree_fresh_detached(&repo_path, &rerun_wt, commit).await?;
        let rerun = self.gates(task, &rerun_wt, &failing, stage).await?;
        self.git.worktree_discard(&repo_path, &rerun_wt).await?;
        Ok(confirm(&first, Some(&rerun)))
    }

    async fn pm_queued(&self, repo: &RepoConfig, check: &PostMergeCheckRow) -> Result<(), PipelineError> {
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let parents = self.git.parent_count(&repo_path, &check.merge_sha).await?;
        match revert_plan(parents, check.commit_count, check.auto_merged) {
            Some(_) => self.advance(check, CheckState::Verifying, CheckPatch::default()).await,
            None => {
                self.block(
                    check,
                    FailureKind::UnsafeMergeStrategy,
                    &format!(
                        "{parents} parent(s), {:?} PR commit(s), auto-merged: {}",
                        check.commit_count, check.auto_merged
                    ),
                )
                .await
            }
        }
    }

    async fn pm_verifying(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let r = self
            .pm_run(task, repo, check, &check.merge_sha, "verify", "post-merge", false)
            .await?;
        if r.failed.is_empty() {
            return self
                .advance(check, CheckState::Passed, CheckPatch { flaky: Some(r.flaky), ..Default::default() })
                .await;
        }
        self.advance(
            check,
            CheckState::VerificationFailed,
            CheckPatch {
                failure_kind: Some(FailureKind::CheckFailed),
                failure_summary: Some(failure_summary(&r.failed)),
                failed_commands: Some(r.failed),
                flaky: Some(r.flaky),
                ..Default::default()
            },
        )
        .await
    }

    // Implemented in Task 6.
    async fn pm_verification_failed(&self, _: &TaskRow, _: &RepoConfig, _: &PostMergeCheckRow) -> Result<(), PipelineError> {
        Ok(())
    }
    async fn pm_preparing_revert(&self, _: &TaskRow, _: &RepoConfig, _: &PostMergeCheckRow) -> Result<(), PipelineError> {
        Ok(())
    }
    // Implemented in Task 7.
    async fn pm_revert_ready(&self, _: &TaskRow, _: &RepoConfig, _: &PostMergeCheckRow) -> Result<(), PipelineError> {
        Ok(())
    }
    // Implemented in Task 8.
    async fn notify_post_merge(&self, _: &TaskRow, _: &RepoConfig, _: &PostMergeCheckRow) -> Result<(), PipelineError> {
        Ok(())
    }
}
```

These three stubs are removed by Tasks 6 to 8; no task after 8 may leave one.

- [ ] **Step 5: Run**

Run: `cargo nextest run --all-features --test post_merge --test scheduler`
Expected: all Task 5 tests and `post_merge_checks_advance_on_every_tick` PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/post_merge.rs crates/provefab/src/pipeline.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: state driver, verifying with one rerun, bounded infra errors, worktree guard

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: `verification_failed` and `preparing_revert`

**Files:**
- Modify: `crates/provefab/src/post_merge.rs`
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Consumes: `pm_run`, `advance`, `block`, `pm_dir` (Task 5); `Git::{fetch, base_ref, rev_parse, is_ancestor, parent_count, worktree_fresh_detached, revert, head, worktree_discard, branch_force}`.
- Produces: rows reaching `superseded`, `preparing_revert`, `revert_ready` (with `base_sha`, `revert_sha`, `revert_branch = "provefab/revert-<id>-<base_moves>"`), or `blocked` with `base_diverged`, `revert_conflict`, `revert_checks_failed`, `dirty_tree`, `unsafe_merge_strategy`.

- [ ] **Step 1: Write the failing tests**

```rust
async fn tick_until(p: &P, id: i64, state: CheckState) {
    for _ in 0..12 {
        if check(p, id).await.state == state {
            return;
        }
        tick(p, id).await;
    }
    panic!("never reached {state:?}");
}

#[tokio::test]
async fn a_fix_already_on_the_base_supersedes_the_revert() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    let fix = commit_and_push(&repo, "README.md", "hello again\n", "fix");
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::Superseded);
    assert_eq!(c.base_sha.as_deref(), Some(fix.as_str()));
    assert!(p.hub.prs.lock().unwrap().is_empty());
    assert_eq!(git(&f.origin, &["branch", "--list", "provefab/*"]), "");
}

#[tokio::test]
async fn a_broken_base_prepares_a_revert_that_passes() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    assert_eq!(c.base_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(c.revert_branch.as_deref(), Some(format!("provefab/revert-{}-0", c.id).as_str()));
    let repo = f.config.repos[0].path_in(&f.home);
    let revert = c.revert_sha.clone().unwrap();
    assert_eq!(git(&repo, &["show", &format!("{revert}:README.md")]), "hello");
    assert_eq!(git(&repo, &["rev-parse", &format!("{revert}^")]), sha);
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_conflicting_revert_is_blocked() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    commit_and_push(&repo, "README.md", "later independent change\n", "later");
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::RevertConflict)));
    assert!(p.hub.prs.lock().unwrap().is_empty());
    assert!(leftovers(&f, c.id).is_empty());
}

#[tokio::test]
async fn a_revert_that_still_fails_is_blocked() {
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::RevertChecksFailed)));
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_check_that_edits_tracked_files_on_the_revert_is_blocked() {
    // Fails on the merge (README is broken), passes on the revert but rewrites README.
    let (_f, p, id, _) = setup(&["grep -q hello README.md && echo changed > README.md"], "broken\n").await;
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::DirtyTree)));
}

#[tokio::test]
async fn a_merge_commit_is_reverted_with_mainline_one() {
    let (f, p, id) = open_pr_task(&["grep -q hello README.md"]).await;
    let repo = f.config.repos[0].path_in(&f.home);
    git(&repo, &["switch", "-qc", "feature"]);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["commit", "-qam", "feature"]);
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha), Some("main"), Some(1));
    p.watch_pr(id).await.unwrap();
    tick_until(&p, id, CheckState::RevertReady).await;
    let revert = check(&p, id).await.revert_sha.unwrap();
    assert_eq!(git(&repo, &["show", &format!("{revert}:README.md")]), "hello");
}

#[tokio::test]
async fn a_merge_no_longer_on_the_base_is_blocked() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    // Someone rewrote main without the merged commit.
    let repo = f.config.repos[0].path_in(&f.home);
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    commit_and_push_force(&repo, "OTHER.md", "x\n", "rewrite");
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::BaseDiverged)));
}

fn commit_and_push_force(repo: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-qm", msg]);
    git(repo, &["push", "-qf", "origin", "main"]);
    git(repo, &["rev-parse", "HEAD"])
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge`
Expected: the new tests FAIL (stubs keep rows in `verification_failed`).

- [ ] **Step 3: Implement**

Replace the two Task 6 stubs in `post_merge.rs` with:

```rust
    async fn pm_verification_failed(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let base_ref = self.git.base_ref(&repo_path, &repo.base).await;
        let tip = self.git.rev_parse(&repo_path, &base_ref).await?;
        if !self.git.is_ancestor(&repo_path, &check.merge_sha, &tip).await? {
            return self
                .block(check, FailureKind::BaseDiverged, &format!("{} is not an ancestor of {tip}", check.merge_sha))
                .await;
        }
        let patch = CheckPatch { base_sha: Some(tip.clone()), ..Default::default() };
        // Nothing landed since the merge: the base is the commit that already failed twice.
        if tip == check.merge_sha {
            return self.advance(check, CheckState::PreparingRevert, patch).await;
        }
        let r = self
            .pm_run(task, repo, check, &tip, "base", "post-merge-base", false)
            .await?;
        let to = if r.failed.is_empty() { CheckState::Superseded } else { CheckState::PreparingRevert };
        self.advance(check, to, patch).await
    }

    async fn pm_preparing_revert(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let base_sha = check
            .base_sha
            .clone()
            .ok_or_else(|| StoreError::Corrupt(format!("check {} has no base_sha", check.id)))?;
        let repo_path = self.checkout(repo);
        let parents = self.git.parent_count(&repo_path, &check.merge_sha).await?;
        let Some(plan) = revert_plan(parents, check.commit_count, check.auto_merged) else {
            return self.block(check, FailureKind::UnsafeMergeStrategy, "merge shape changed").await;
        };
        let wt = self.pm_dir(check.id, "revert");
        self.git.worktree_fresh_detached(&repo_path, &wt, &base_sha).await?;
        match self.git.revert(&wt, &check.merge_sha, plan.mainline()).await {
            Ok(()) => {}
            // git exits 1 on a conflict; anything else is infrastructure.
            Err(ForgeError::Failed { code: Some(1), stderr, .. }) => {
                return self.block(check, FailureKind::RevertConflict, &stderr).await;
            }
            Err(e) => return Err(e.into()),
        }
        let revert_sha = self.git.head(&wt).await?;
        self.git.worktree_discard(&repo_path, &wt).await?;
        let branch = format!("provefab/revert-{}-{}", check.id, check.base_moves);
        self.git.branch_force(&repo_path, &branch, &revert_sha).await?;
        let r = self
            .pm_run(task, repo, check, &revert_sha, "revert-check", "revert-check", true)
            .await?;
        if r.dirty {
            return self.block(check, FailureKind::DirtyTree, "a check modified tracked files on the revert").await;
        }
        if !r.failed.is_empty() {
            return self
                .block(check, FailureKind::RevertChecksFailed, &failure_summary(&r.failed))
                .await;
        }
        self.advance(
            check,
            CheckState::RevertReady,
            CheckPatch { revert_sha: Some(revert_sha), revert_branch: Some(branch), ..Default::default() },
        )
        .await
    }
```

`stderr` goes only to the local, bounded `failure_summary`; `render` never publishes it (Task 2 test).

- [ ] **Step 4: Run**

Run: `cargo nextest run --all-features --test post_merge`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/post_merge.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: current-base check (superseded) and revert preparation per merge shape

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: `revert_ready`: base race, idempotent push and PR

**Files:**
- Modify: `crates/provefab/src/post_merge.rs`
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Consumes: `Git::{fetch, base_ref, rev_parse, remote_branch_sha, push_sha}`, `Hub::{pr_create, pr_status}`, `revert_pr_body` (Task 2), `BASE_MOVE_LIMIT`.
- Produces: rows reaching `revert_open` (with `revert_pr_url`), back to `verification_failed` (base moved, `base_moves + 1`), or `blocked` with `base_moved`, `branch_conflict`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn a_failing_merge_opens_exactly_one_human_reviewed_revert() {
    let (f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    let c = drive(&p, id).await;
    assert_eq!(c.state, CheckState::RevertOpen);
    assert_eq!(c.revert_pr_url.as_deref(), Some("https://github.com/o/r/pull/100"));
    let prs = p.hub.prs.lock().unwrap().clone();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].0, format!("provefab/revert-{}-0", c.id));
    assert_eq!(prs[0].1, "main");
    assert!(prs[0].3.contains(&sha) && prs[0].3.contains("Provefab will not merge this revert automatically."));
    assert_eq!(
        git(&f.origin, &["rev-parse", &format!("refs/heads/{}", prs[0].0)]),
        c.revert_sha.clone().unwrap()
    );
    // Local branch cleaned up; nothing merged.
    let repo = f.config.repos[0].path_in(&f.home);
    assert_eq!(git(&repo, &["branch", "--list", "provefab/revert-*"]), "");
    assert!(p.hub.merged.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_pr_creation_error_resumes_without_a_second_branch_or_pr() {
    use std::sync::atomic::Ordering;
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    p.hub.pr_create_failures.store(2, Ordering::SeqCst);
    assert!(p.process_post_merge(id).await.is_err());
    assert!(p.process_post_merge(id).await.is_err());
    let c = check(&p, id).await;
    assert_eq!((c.state, c.infra_errors), (CheckState::RevertReady, 2));
    let pushed = git(&f.origin, &["rev-parse", &format!("refs/heads/provefab/revert-{}-0", c.id)]);
    assert_eq!(tick(&p, id).await, CheckState::RevertOpen);
    assert_eq!(check(&p, id).await.infra_errors, 0);
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
    assert_eq!(git(&f.origin, &["rev-parse", &format!("refs/heads/provefab/revert-{}-0", c.id)]), pushed);
}

#[tokio::test]
async fn five_infra_errors_block_the_check() {
    use std::sync::atomic::Ordering;
    let (_f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    p.hub.pr_create_failures.store(10, Ordering::SeqCst);
    for _ in 0..4 {
        assert!(p.process_post_merge(id).await.is_err());
    }
    p.process_post_merge(id).await.unwrap();
    let c = check(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::InfraError)));
}

#[tokio::test]
async fn a_moving_base_restarts_then_blocks_on_the_third_move() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    let repo = f.config.repos[0].path_in(&f.home);
    for n in 0..2 {
        tick_until(&p, id, CheckState::RevertReady).await;
        commit_and_push(&repo, &format!("OTHER{n}.md"), "x\n", "unrelated");
        assert_eq!(tick(&p, id).await, CheckState::VerificationFailed);
        assert_eq!(check(&p, id).await.base_moves, n + 1);
    }
    tick_until(&p, id, CheckState::RevertReady).await;
    assert_eq!(check(&p, id).await.revert_branch.unwrap(), format!("provefab/revert-{}-2", check(&p, id).await.id));
    commit_and_push(&repo, "OTHER2.md", "x\n", "unrelated");
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::BaseMoved)));
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_foreign_commit_on_the_revert_branch_blocks() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    git(&repo, &["push", "-q", "origin", &format!("HEAD~1:refs/heads/{}", c.revert_branch.clone().unwrap())]);
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::BranchConflict)));
    assert!(p.hub.prs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_reused_pr_on_other_work_blocks() {
    let (_f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    *p.hub.pr_head_override.lock().unwrap() = Some("0".repeat(40));
    let c = drive(&p, id).await;
    assert_eq!((c.state, c.failure_kind), (CheckState::Blocked, Some(FailureKind::BranchConflict)));
}

#[tokio::test]
async fn a_crash_after_the_push_reuses_the_pushed_branch() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::RevertReady).await;
    let c = check(&p, id).await;
    let repo = f.config.repos[0].path_in(&f.home);
    p.git.push_sha(&repo, c.revert_sha.as_deref().unwrap(), c.revert_branch.as_deref().unwrap()).await.unwrap();
    assert_eq!(drive(&p, id).await.state, CheckState::RevertOpen);
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_innocent_later_merge_is_never_reverted() {
    // Merge A breaks README; its check opens a revert PR (not merged).
    let (f, p, a, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    assert_eq!(drive(&p, a).await.state, CheckState::RevertOpen);
    // Merge B only adds a file; the base is still broken by A.
    let b = provefab::testkit::queue_n(&p, 9, "Second change").await;
    let repo = f.config.repos[0].path_in(&f.home);
    let sha_b = commit_and_push(&repo, "B.md", "b\n", "innocent");
    p.store.set_pr(b, "https://github.com/o/r/pull/9", "open").await.unwrap();
    p.store.transition(b, TaskState::PrOpen, "pr").await.unwrap();
    *p.hub.pr_status.lock().unwrap() = merged(Some(&sha_b), Some("main"), Some(1));
    p.watch_pr(b).await.unwrap();
    let cb = drive(&p, b).await;
    assert_eq!((cb.state, cb.failure_kind), (CheckState::Blocked, Some(FailureKind::RevertChecksFailed)));
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1, "only the culprit has a revert PR");
}
```

This pins Review Focus 3: reverting B does not restore the checks, so B is blocked, not reverted.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge`
Expected: FAIL (rows stay `revert_ready`).

- [ ] **Step 3: Implement**

Replace the Task 7 stub:

```rust
    async fn pm_revert_ready(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("check {} has no {what}", check.id));
        let base_sha = check.base_sha.clone().ok_or_else(|| corrupt("base_sha"))?;
        let revert_sha = check.revert_sha.clone().ok_or_else(|| corrupt("revert_sha"))?;
        let branch = check.revert_branch.clone().ok_or_else(|| corrupt("revert_branch"))?;
        let repo_path = self.checkout(repo);
        self.git.fetch(&repo_path).await?;
        let base_ref = self.git.base_ref(&repo_path, &repo.base).await;
        let tip = self.git.rev_parse(&repo_path, &base_ref).await?;
        if tip != base_sha {
            if check.base_moves + 1 >= BASE_MOVE_LIMIT {
                return self
                    .block(check, FailureKind::BaseMoved, &format!("base moved {} times", check.base_moves + 1))
                    .await;
            }
            return self
                .advance(
                    check,
                    CheckState::VerificationFailed,
                    CheckPatch { bump_base_moves: true, ..Default::default() },
                )
                .await;
        }
        match self.git.remote_branch_sha(&repo_path, &branch).await? {
            None => self.git.push_sha(&repo_path, &revert_sha, &branch).await?,
            Some(s) if s == revert_sha => {}
            Some(s) => {
                return self
                    .block(check, FailureKind::BranchConflict, &format!("origin/{branch} is {s}, expected {revert_sha}"))
                    .await;
            }
        }
        let body = revert_pr_body(task.pr_url.as_deref().unwrap_or("unknown"), &task.issue_url, check);
        let title = format!("Revert Provefab change {}", &check.merge_sha[..check.merge_sha.len().min(12)]);
        let url = self
            .hub
            .pr_create(&repo.slug, &branch, &repo.base, &title, &body)
            .await?;
        let head = self.hub.pr_status(&repo.slug, &url).await?.head_sha;
        if head.as_deref() != Some(revert_sha.as_str()) {
            return self
                .block(check, FailureKind::BranchConflict, &format!("{url} head is {head:?}, expected {revert_sha}"))
                .await;
        }
        self.advance(
            check,
            CheckState::RevertOpen,
            CheckPatch { revert_pr_url: Some(url), ..Default::default() },
        )
        .await
    }
```

If `task.issue_url` has another name on `TaskRow`, use the field `commands::log` prints for the issue URL.

- [ ] **Step 4: Run**

Run: `cargo nextest run --all-features --test post_merge`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/post_merge.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: revert_ready with base-race restart, exact-sha push and PR head check

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Notifications

**Files:**
- Modify: `crates/provefab/src/post_merge.rs`
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Consumes: `render`, `marker` (Task 2); `mark_post_merge_notified`, `NoticeTarget` (Task 1); `Hub::{comments, pr_status, pr_comment}`; `Pipeline::tell`.
- Produces: at most one comment per target per check; per-target give-up after `INFRA_ERROR_LIMIT` consecutive failures.

- [ ] **Step 1: Write the failing tests**

```rust
fn issue_markers(p: &P, check_id: i64) -> usize {
    let m = provefab::post_merge::marker(check_id);
    p.hub.comments.lock().unwrap().iter().filter(|c| c.body.contains(&m)).count()
}

fn pr_markers(p: &P, check_id: i64) -> usize {
    let m = provefab::post_merge::marker(check_id);
    p.hub.pr_statuses.lock().unwrap()["https://github.com/o/r/pull/8"]
        .comments
        .iter()
        .filter(|c| c.body.contains(&m))
        .count()
}

#[tokio::test]
async fn a_failed_pr_comment_is_retried_without_a_second_issue_comment() {
    use std::sync::atomic::Ordering;
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    p.hub.pr_comment_failures.store(1, Ordering::SeqCst);
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let c = check(&p, id).await;
    assert_eq!(c.state, CheckState::Blocked);
    assert_eq!((issue_markers(&p, c.id), pr_markers(&p, c.id)), (1, 1));
    assert!(c.issue_notified_at.is_some() && c.pr_notified_at.is_some());
}

#[tokio::test]
async fn a_target_that_keeps_failing_is_given_up_after_the_limit() {
    use std::sync::atomic::Ordering;
    // No revert PR is ever created here, so every PR comment is the notice.
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    p.hub.pr_comment_failures.store(1000, Ordering::SeqCst);
    let mut errors = 0;
    for _ in 0..20 {
        if p.process_post_merge(id).await.is_err() {
            errors += 1;
        }
    }
    let c = check(&p, id).await;
    assert_eq!(c.state, CheckState::Blocked);
    // The blocking transition reset the counter: 4 errors, then the 5th gives up.
    assert_eq!(errors, provefab::post_merge::INFRA_ERROR_LIMIT as usize - 1);
    assert!(c.pr_notified_at.is_some());
    assert_eq!(pr_markers(&p, c.id), 0);
    assert_eq!(issue_markers(&p, c.id), 1);
}

#[tokio::test]
async fn each_check_is_announced_by_its_own_id() {
    let (_f, p, id, _) = setup(&["false"], "broken\n").await;
    let second = p
        .store
        .ensure_post_merge_check(&provefab::store::NewPostMergeCheck {
            task_id: id,
            merge_sha: "unknown",
            base: "main",
            commit_count: None,
            auto_merged: false,
        })
        .await
        .unwrap();
    p.store
        .advance_post_merge(
            second.id,
            CheckState::Queued,
            CheckState::Blocked,
            &provefab::store::CheckPatch {
                failure_kind: Some(FailureKind::AttributionMissing),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let first = check(&p, id).await;
    assert_eq!((issue_markers(&p, first.id), issue_markers(&p, second.id)), (1, 1));
    assert_eq!((pr_markers(&p, first.id), pr_markers(&p, second.id)), (1, 1));
}

#[tokio::test]
async fn nothing_published_contains_command_output_or_local_paths() {
    // The output joins to SENTINEL_SECRET_42; the command text never contains it.
    let (f, p, id, _) = setup(&["printf %s SENTINEL_; printf %s SECRET_42; false"], "broken\n").await;
    drive(&p, id).await;
    tick(&p, id).await;
    let home = f.home.display().to_string();
    let mut published: Vec<String> = p.hub.posted.lock().unwrap().clone();
    published.extend(p.hub.prs.lock().unwrap().iter().map(|pr| pr.3.clone()));
    assert!(!published.is_empty());
    for body in published {
        assert!(!body.contains("SENTINEL_SECRET_42"), "{body}");
        assert!(!body.contains(&home), "{body}");
    }
}

#[tokio::test]
async fn a_superseded_check_is_announced_once() {
    let (f, p, id, _) = setup(&["grep -q hello README.md"], "broken\n").await;
    tick_until(&p, id, CheckState::VerificationFailed).await;
    let repo = f.config.repos[0].path_in(&f.home);
    commit_and_push(&repo, "README.md", "hello again\n", "fix");
    drive(&p, id).await;
    tick(&p, id).await;
    tick(&p, id).await;
    let c = check(&p, id).await;
    assert_eq!((issue_markers(&p, c.id), pr_markers(&p, c.id)), (1, 1));
}
```

Note on `tell`: it goes through the pending GitHub-effect queue (`github(...)` in `pipeline.rs`). If the FakeHub's `comments` list only receives the comment when the effect is flushed, flush it the way `tests/scheduler.rs::pending_github_comment_is_retried_on_next_poll` does, or assert on `p.hub.posted` instead of `p.hub.comments`. Read that test before writing `issue_markers`, and pick the observable that reflects a posted issue comment.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --all-features --test post_merge`
Expected: FAIL (no comments posted; stub).

- [ ] **Step 3: Implement**

Replace the Task 8 stub:

```rust
    /// One comment per target per check, from fixed templates only (spec section 7).
    /// The marker makes a crash between the GitHub write and the SQLite write
    /// harmless; `INFRA_ERROR_LIMIT` consecutive failures give a target up.
    async fn notify_post_merge(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        check: &PostMergeCheckRow,
    ) -> Result<(), PipelineError> {
        let Some(body) = render(check) else {
            return Ok(());
        };
        let mark = marker(check.id);
        if check.issue_notified_at.is_none() {
            let posted = async {
                let seen = self
                    .hub
                    .comments(&repo.slug, task.issue_number)
                    .await?
                    .iter()
                    .any(|c| c.body.contains(&mark));
                if !seen {
                    self.tell(task.id, &repo.slug, task.issue_number, &body).await?;
                }
                Ok::<(), PipelineError>(())
            }
            .await;
            self.settle_notice(check, NoticeTarget::Issue, posted).await?;
        }
        if check.pr_notified_at.is_none() {
            let Some(url) = task.pr_url.as_deref() else {
                self.store.mark_post_merge_notified(check.id, NoticeTarget::Pr).await?;
                return Ok(());
            };
            let posted = async {
                let seen = self
                    .hub
                    .pr_status(&repo.slug, url)
                    .await?
                    .comments
                    .iter()
                    .any(|c| c.body.contains(&mark));
                if !seen {
                    self.hub.pr_comment(&repo.slug, url, &body).await?;
                }
                Ok::<(), PipelineError>(())
            }
            .await;
            self.settle_notice(check, NoticeTarget::Pr, posted).await?;
        }
        Ok(())
    }

    /// Marks a target done on success; on failure counts it and gives the
    /// target up at the limit (logged, returns `Ok`), else returns the error.
    async fn settle_notice(
        &self,
        check: &PostMergeCheckRow,
        target: NoticeTarget,
        posted: Result<(), PipelineError>,
    ) -> Result<(), PipelineError> {
        match posted {
            Ok(()) => {
                self.store.mark_post_merge_notified(check.id, target).await?;
                Ok(())
            }
            Err(e) => {
                let n = self.store.post_merge_infra_error(check.id).await?;
                if n >= INFRA_ERROR_LIMIT {
                    eprintln!("provefab: gave up notifying {target:?} for post-merge check {}: {e}", check.id);
                    self.store.mark_post_merge_notified(check.id, target).await?;
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }
```

Notices share the `infra_errors` counter. It is 0 when a check becomes terminal (the transition reset it in `advance_post_merge`), so the give-up bound counts notice failures only.

- [ ] **Step 4: Run**

Run: `cargo nextest run --all-features --test post_merge`
Expected: PASS. Confirm no stub remains: `grep -n "Implemented in Task" crates/provefab/src/post_merge.rs` prints nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/post_merge.rs crates/provefab/tests/post_merge.rs
git commit -m "post-merge: per-target notices by check id, marker dedupe, bounded give-up

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: CLI output and docs

**Files:**
- Modify: `crates/provefab/src/commands.rs`, `docs/guide/configuration.md`, `docs/guide/usage.md`, `docs/guide/operations.md`, `README.md`, `provefab.example.toml`
- Test: `crates/provefab/tests/post_merge.rs`

**Interfaces:**
- Consumes: `Store::{post_merge_checks, post_merge_state_counts}`, `PostMergeCheckRow` fields.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn status_log_and_stats_show_the_check() {
    let (_f, p, id, sha) = setup(&["grep -q hello README.md"], "broken\n").await;
    let c = drive(&p, id).await;
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(status.contains("post-merge checks: revert_open 1"), "{status}");
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    for needle in [
        "revert_open".to_string(),
        sha.clone(),
        c.base_sha.clone().unwrap(),
        c.revert_sha.clone().unwrap(),
        "https://github.com/o/r/pull/100".to_string(),
        "`grep -q hello README.md` exited with 1".to_string(),
    ] {
        assert!(log.contains(&needle), "{needle} missing from:\n{log}");
    }
    let stats = provefab::commands::stats(&p.store).await.unwrap();
    assert!(
        stats.contains("post-merge passed 0 · flaky 0 · superseded 0 · reverts opened 1 · blocked 0"),
        "{stats}"
    );
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo nextest run --all-features --test post_merge status_log_and_stats_show_the_check`
Expected: FAIL.

- [ ] **Step 3: Implement**

`status`: remove the v1 per-task `post_merge` suffix; after the task lines, append:

```rust
    let counts = store.post_merge_state_counts().await?;
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(s, n)| format!("{} {n}", s.as_str())).collect();
        let _ = writeln!(out, "post-merge checks: {}", parts.join(" · "));
    }
```

`log`: replace the v1 post-merge block with:

```rust
    let post_merge = store.post_merge_checks(id).await?;
    if !post_merge.is_empty() {
        out.push_str("\npost-merge checks:\n");
        for c in post_merge {
            let _ = writeln!(
                out,
                "  #{} {} {} on {}{}",
                c.id,
                c.state.as_str(),
                c.merge_sha,
                c.base,
                c.failure_kind.map(|k| format!("  ({})", k.as_str())).unwrap_or_default()
            );
            for (label, v) in [("base tip", &c.base_sha), ("revert commit", &c.revert_sha), ("revert PR", &c.revert_pr_url)] {
                if let Some(v) = v {
                    let _ = writeln!(out, "    {label}: {v}");
                }
            }
            if !c.failed_commands.is_empty() {
                let _ = writeln!(out, "    failed:\n{}", crate::post_merge::failure_summary(&c.failed_commands).lines().map(|l| format!("      {l}")).collect::<Vec<_>>().join("\n"));
            }
            if !c.flaky.is_empty() {
                let _ = writeln!(out, "    flaky: {}", c.flaky.join(", "));
            }
            if let Some(s) = &c.failure_summary {
                let _ = writeln!(out, "    detail: {s}");
            }
        }
    }
```

`stats`: replace the three v1 counters with `post_merge_passed`, `post_merge_flaky`, `post_merge_superseded`, `revert_prs`, `post_merge_blocked` (all `u32`), counted as:

```rust
        for check in store.post_merge_checks(t.id).await? {
            match check.state {
                CheckState::Passed => {
                    r.post_merge_passed += 1;
                    if !check.flaky.is_empty() {
                        r.post_merge_flaky += 1;
                    }
                }
                CheckState::Superseded => r.post_merge_superseded += 1,
                CheckState::RevertOpen => r.revert_prs += 1,
                CheckState::Blocked => r.post_merge_blocked += 1,
                _ => {}
            }
        }
```

and print `· post-merge passed {} · flaky {} · superseded {} · reverts opened {} · blocked {}` in that order in the per-repo line.

- [ ] **Step 4: Docs**

- `docs/guide/usage.md` item 9, replace with: "**Optional post-merge verification.** If you set `post_merge_checks`, Provefab runs them on the exact commit its PR produced on the base branch. A failing command is rerun once on a fresh checkout; if it passes then, the run counts as passed and the command is reported as flaky. On a confirmed failure, Provefab runs the same checks on the current base: if they pass, a later commit already fixed it and no revert is proposed. Otherwise Provefab prepares a revert on the current base and opens a revert PR only if the reverted tree passes the same checks. Revert conflicts, failing revert checks, multi-commit PRs merged by rebase, and a base branch that keeps moving require a human. Provefab never merges a revert PR. These are repository commands, not production monitoring."
- `docs/guide/usage.md` log bullet: mention state, failure kind, base tip, revert commit, revert PR, failed and flaky commands.
- `docs/guide/configuration.md` `post_merge_checks` row: append "Command strings may appear in GitHub comments: never put secrets on the command line, use the environment."
- `docs/guide/operations.md`: `~/.provefab/post-merge/` row: "temporary detached worktrees, one per check step, removed when the step ends"; add a line that remote `provefab/revert-*` branches are never deleted by Provefab.
- `README.md`: in the post-merge sentence, add "unless a later commit already fixed it" after "on failure it can open a human-reviewed revert PR".
- `provefab.example.toml`: keep the example; add a comment line `# Never put secrets in these commands: failing command lines are posted on GitHub.`

Check: `grep -rn "exactly one commit\|multiple commits" docs README.md` returns nothing outdated (v1 said multi-commit PRs always need a human; v2 allows squash-merged ones by Provefab).

- [ ] **Step 5: Run everything**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: PASS. Also run `cargo test --doc` if the crate has doc tests.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/commands.rs crates/provefab/tests/post_merge.rs docs/guide README.md provefab.example.toml
git commit -m "post-merge: status counts, detailed log, stats per outcome; docs for rev 2

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Final verification, real run, ledger

**Files:**
- Modify: `docs/handoff-2026-09-29-feature1-post-merge.md` (ledger), spec status line.

- [ ] **Step 1: Full checks**

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --all-features
```

All three must pass. Paste the summary lines (tests passed/skipped) into the ledger.

- [ ] **Step 2: Spec coverage sweep**

For each row of spec section 12 "Tests", name the test that covers it in the ledger. Any row without a test: add the test to the owning task's file and rerun.

- [ ] **Step 3: Real run (needs the owner's explicit go: it creates a GitHub repository)**

Ask the owner for: a disposable private repo name, and the go. Then, with a `provefab.toml` pointing at it and `post_merge_checks = ["grep -q hello README.md"]`:

1. A Provefab PR that keeps README containing `hello`, merged by hand (squash): `provefab log <id>` shows `passed`.
2. A Provefab PR that removes `hello`, merged (squash): a revert PR opens, not merged; `provefab log <id>` shows `revert_open`, base tip, revert commit, PR URL; the PR body and the issue/PR comments contain no local path.
3. Same as 2, but push a fix to `main` between the verifying and the base-check ticks: `superseded`, one comment on each of issue and PR, no revert PR.

Save `provefab log` output and URLs in the ledger. Without the go, stop here and record "real run not done: awaiting owner's go".

- [ ] **Step 4: Ledger and spec status**

In `docs/handoff-2026-09-29-feature1-post-merge.md`, add a dated section "v2 state machine" listing: commits, check results, the coverage map, the real-run evidence (or why missing), and which of the 12 review concerns are now closed (1 to 9 and 12 by this plan; 10 and 11 stay release steps). Set the spec status line to "revision 2 implemented; release pending owner's go".

- [ ] **Step 5: Commit**

```bash
git add docs/handoff-2026-09-29-feature1-post-merge.md docs/specs/2026-09-29-post-merge-verification-design.md
git commit -m "docs: post-merge v2 ledger and evidence

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Release steps (spec section 14) are out of this plan; each needs the owner's go.
