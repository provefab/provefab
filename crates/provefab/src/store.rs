//! Durable task state in SQLite (spec §3.5). Every state change is written,
//! with its reason, before the side effect it leads to (spec §3.2).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Row, SqliteConnection};

use crate::post_merge::{CheckState, FailedCommand, FailureKind, bounded};
use crate::record::{Event, FindingRow, RULE_VERSION, Rule, StoredEvent};
use crate::task::{TaskKind, TaskState};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store: {0}")]
    Db(#[from] sqlx::Error),
    #[error("store: migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("store: no task {0}")]
    UnknownTask(i64),
    #[error("store: unreadable value `{0}` in the database")]
    Corrupt(String),
}

/// An issue as intake found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIssue {
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: i64,
    pub repo: String,
    pub issue_number: u64,
    pub issue_url: String,
    pub title: String,
    pub author: String,
    pub state: TaskState,
    pub kind: Option<TaskKind>,
    pub attempts: u32,
    pub review_rounds: u32,
    pub branch: Option<String>,
    pub worktree: Option<PathBuf>,
    pub pr_url: Option<String>,
    pub pr_state: Option<String>,
    pub reopen_count: u32,
    /// The retry ladder moved the implement stage up one tier (spec §3.2).
    pub escalated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRow {
    pub from: Option<TaskState>,
    pub to: TaskState,
    pub reason: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostMergeCheckRow {
    pub id: i64,
    pub task_id: i64,
    pub merge_sha: String,
    pub base: String,
    /// The merged pull request, not the task's current one.
    pub pr_url: Option<String>,
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
    pub pr_url: Option<&'a str>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct StageRunRecord {
    pub task_id: i64,
    pub stage: String,
    pub model_id: String,
    pub exit: String,
    pub turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub session_dir: PathBuf,
    pub gate_score: Option<String>,
    pub started_at: i64,
    pub finished_at: i64,
    /// Cost per stage (D74).
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub actual_model: Option<String>,
    pub cost_usd: Option<f64>,
    pub quota_units: Option<f64>,
}

pub struct Store {
    pool: SqlitePool,
}

/// Writes that ride along with a transition, in its transaction.
#[derive(Debug, Clone, Copy)]
pub enum Also<'a> {
    Nothing,
    /// A stage starts: fresh attempt counter and tier.
    NewStage,
    /// A review asked for changes: one more round, fresh attempts and tier.
    NewRound,
    /// Provefab asks for information: keep the question, and count only
    /// comments after `seen_at` (RFC 3339 UTC) as replies.
    Question {
        question: &'a Value,
        seen_at: &'a str,
    },
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`, GitHub's timestamp format.
pub fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    /// Opens (creating if needed) the database and applies the migrations.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Queues an issue. `None` when the issue URL is already known (intake is idempotent).
    pub async fn add_issue(&self, issue: &NewIssue) -> Result<Option<i64>, StoreError> {
        // IMMEDIATE takes the write lock up front: a deferred transaction that reads
        // first gets SQLITE_BUSY when another writer commits in between (review C1).
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let t = now();
        let inserted = sqlx::query(
            "INSERT INTO tasks (repo, issue_number, issue_url, title, author, state, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (repo, issue_number) DO NOTHING",
        )
        // GitHub slugs are case-insensitive; (repo, number) is the issue's identity.
        .bind(issue.repo.to_lowercase())
        .bind(issue.number as i64)
        .bind(&issue.url)
        .bind(&issue.title)
        .bind(&issue.author)
        .bind(TaskState::Queued.as_str())
        .bind(t)
        .bind(t)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 0 {
            return Ok(None);
        }
        let id = inserted.last_insert_rowid();
        sqlx::query("INSERT INTO transitions (task_id, from_state, to_state, reason, at) VALUES (?, NULL, ?, 'intake', ?)")
            .bind(id)
            .bind(TaskState::Queued.as_str())
            .bind(t)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    pub async fn task(&self, id: i64) -> Result<Option<TaskRow>, StoreError> {
        let row = sqlx::query("SELECT * FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| task_row(&r)).transpose()
    }

    pub async fn task_by_url(&self, url: &str) -> Result<Option<TaskRow>, StoreError> {
        let row = sqlx::query("SELECT * FROM tasks WHERE issue_url = ?")
            .bind(url)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| task_row(&r)).transpose()
    }

    /// Tasks in any of `states`, oldest first.
    pub async fn tasks_in(&self, states: &[TaskState]) -> Result<Vec<TaskRow>, StoreError> {
        let mut out = Vec::new();
        for state in states {
            let rows = sqlx::query("SELECT * FROM tasks WHERE state = ?")
                .bind(state.as_str())
                .fetch_all(&self.pool)
                .await?;
            for r in rows {
                out.push(task_row(&r)?);
            }
        }
        out.sort_by_key(|t| t.id);
        Ok(out)
    }

    /// Moves a task to `to` and logs the change in the same transaction.
    pub async fn transition(&self, id: i64, to: TaskState, reason: &str) -> Result<(), StoreError> {
        self.transition_and(id, to, reason, Also::Nothing).await
    }

    /// A transition plus the writes that must land with it, all or nothing
    /// (Plan 3b review I1, I2): a crash between them used to leave half a step.
    pub async fn transition_and(
        &self,
        id: i64,
        to: TaskState,
        reason: &str,
        also: Also<'_>,
    ) -> Result<(), StoreError> {
        // IMMEDIATE takes the write lock up front: a deferred transaction that reads
        // first gets SQLITE_BUSY when another writer commits in between (review C1).
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let from: Option<String> = sqlx::query("SELECT state FROM tasks WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.get("state"));
        let Some(from) = from else {
            return Err(StoreError::UnknownTask(id));
        };
        let t = now();
        sqlx::query("UPDATE tasks SET state = ?, updated_at = ? WHERE id = ?")
            .bind(to.as_str())
            .bind(t)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO transitions (task_id, from_state, to_state, reason, at) VALUES (?, ?, ?, ?, ?)")
            .bind(id)
            .bind(from)
            .bind(to.as_str())
            .bind(reason)
            .bind(t)
            .execute(&mut *tx)
            .await?;
        match also {
            Also::Nothing => {}
            Also::NewStage => {
                sqlx::query("UPDATE tasks SET attempts = 0, escalated = 0 WHERE id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            Also::NewRound => {
                sqlx::query(
                    "UPDATE tasks SET attempts = 0, escalated = 0, review_rounds = review_rounds + 1 WHERE id = ?",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            Also::Question { question, seen_at } => {
                sqlx::query(
                    "INSERT INTO stage_outputs (task_id, kind, json, at) VALUES (?, 'question', ?, ?)",
                )
                .bind(id)
                .bind(question.to_string())
                .bind(t)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO replies_seen (task_id, last_comment_at) VALUES (?, ?)
                     ON CONFLICT (task_id) DO UPDATE SET last_comment_at = excluded.last_comment_at",
                )
                .bind(id)
                .bind(seen_at)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn transitions(&self, id: i64) -> Result<Vec<TransitionRow>, StoreError> {
        let rows = sqlx::query("SELECT from_state, to_state, reason, at FROM transitions WHERE task_id = ? ORDER BY id")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let from: Option<String> = r.get("from_state");
                Ok(TransitionRow {
                    from: from.map(|f| parse_state(&f)).transpose()?,
                    to: parse_state(&r.get::<String, _>("to_state"))?,
                    reason: r.get("reason"),
                    at: r.get("at"),
                })
            })
            .collect()
    }

    pub async fn set_kind(&self, id: i64, kind: TaskKind) -> Result<(), StoreError> {
        self.update(
            id,
            "UPDATE tasks SET kind = ?, updated_at = ? WHERE id = ?",
            |q| q.bind(kind.as_str()),
        )
        .await
    }

    pub async fn set_worktree(
        &self,
        id: i64,
        branch: &str,
        worktree: &Path,
    ) -> Result<(), StoreError> {
        let path = worktree.display().to_string();
        self.update(
            id,
            "UPDATE tasks SET branch = ?, worktree = ?, updated_at = ? WHERE id = ?",
            |q| q.bind(branch.to_string()).bind(path),
        )
        .await
    }

    /// Adds one to the attempt counter and returns the new value.
    pub async fn bump_attempts(&self, id: i64) -> Result<u32, StoreError> {
        self.bump(id, Counter::Attempts).await
    }

    pub async fn reset_attempts(&self, id: i64) -> Result<(), StoreError> {
        self.update(
            id,
            "UPDATE tasks SET attempts = 0, updated_at = ? WHERE id = ?",
            |q| q,
        )
        .await
    }

    /// Adds one to the review-round counter and returns the new value.
    pub async fn bump_review_rounds(&self, id: i64) -> Result<u32, StoreError> {
        self.bump(id, Counter::ReviewRounds).await
    }

    /// Links the task to its pull request (spec §3.4 item 7).
    pub async fn set_pr(&self, id: i64, url: &str, state: &str) -> Result<(), StoreError> {
        self.write_with_events(id, Write::SetPr { url, state }, &[])
            .await
            .map(|_| ())
    }

    pub async fn set_pr_state(&self, id: i64, state: &str) -> Result<(), StoreError> {
        self.write_with_events(id, Write::SetPrState(state), &[])
            .await
            .map(|_| ())
    }

    /// Creates the one check for a merged commit, or returns the existing row
    /// when a scheduler retry sees the merge again.
    pub async fn ensure_post_merge_check(
        &self,
        new: &NewPostMergeCheck<'_>,
    ) -> Result<PostMergeCheckRow, StoreError> {
        sqlx::query(
            "INSERT INTO post_merge_checks (task_id, merge_sha, base, pr_url, commit_count, auto_merged, state) \
             VALUES (?, ?, ?, ?, ?, ?, 'queued') ON CONFLICT (task_id, merge_sha) DO NOTHING",
        )
        .bind(new.task_id)
        .bind(new.merge_sha)
        .bind(new.base)
        .bind(new.pr_url)
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
        let failed_commands = patch
            .failed_commands
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let flaky = patch
            .flaky
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let at = now();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
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
        .bind(failed_commands)
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
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(false);
        }
        if to.is_terminal() {
            let (task_id, stored_kind): (i64, Option<String>) =
                sqlx::query_as("SELECT task_id, failure_kind FROM post_merge_checks WHERE id = ?")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            let failure_kind = patch
                .failure_kind
                .map(|k| k.as_str().to_string())
                .or(stored_kind);
            append_event(
                &mut tx,
                task_id,
                &Event::PostMerge {
                    check_id: id,
                    state: to.as_str().into(),
                    failure_kind,
                },
            )
            .await?;
            if to == CheckState::RevertOpen {
                let pass: Option<i64> = sqlx::query_scalar(
                    "SELECT json_extract(payload, '$.pass') FROM change_events \
                     WHERE task_id = ? AND kind = 'merged' ORDER BY seq DESC LIMIT 1",
                )
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
                if let Some(pass) = pass {
                    infer_findings(&mut tx, task_id, Rule::FollowedByRevert, pass as u32).await?;
                }
            }
        }
        tx.commit().await?;
        Ok(true)
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

    pub async fn reset_post_merge_infra_errors(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE post_merge_checks SET infra_errors = 0 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_post_merge_notified(
        &self,
        id: i64,
        target: NoticeTarget,
    ) -> Result<(), StoreError> {
        let sql = match target {
            NoticeTarget::Issue => {
                "UPDATE post_merge_checks SET issue_notified_at = ? WHERE id = ?"
            }
            NoticeTarget::Pr => "UPDATE post_merge_checks SET pr_notified_at = ? WHERE id = ?",
        };
        sqlx::query(sql)
            .bind(now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Tasks with a check still to advance, or a failed check with a notice
    /// still to post, whatever the task's own state (final review F2).
    pub async fn tasks_with_open_post_merge(&self) -> Result<Vec<i64>, StoreError> {
        // The terminal states, as pinned by `CheckState::is_terminal` and its test.
        let rows = sqlx::query(
            "SELECT DISTINCT task_id FROM post_merge_checks \
             WHERE state NOT IN (?, ?, ?, ?) \
             OR (state <> ? AND (issue_notified_at IS NULL OR pr_notified_at IS NULL)) \
             ORDER BY task_id",
        )
        .bind(CheckState::Passed.as_str())
        .bind(CheckState::Superseded.as_str())
        .bind(CheckState::RevertOpen.as_str())
        .bind(CheckState::Blocked.as_str())
        .bind(CheckState::Passed.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("task_id")).collect())
    }

    /// `(state, count)` over every check, in `CheckState::ALL` order, zeros omitted.
    pub async fn post_merge_state_counts(&self) -> Result<Vec<(CheckState, i64)>, StoreError> {
        let rows = sqlx::query("SELECT state, COUNT(*) AS n FROM post_merge_checks GROUP BY state")
            .fetch_all(&self.pool)
            .await?;
        let mut counts = Vec::new();
        for s in CheckState::ALL {
            if let Some(r) = rows
                .iter()
                .find(|r| r.get::<String, _>("state") == s.as_str())
            {
                counts.push((s, r.get::<i64, _>("n")));
            }
        }
        Ok(counts)
    }

    /// The issue came back after its PR was merged.
    pub async fn record_reopen(&self, id: i64) -> Result<u32, StoreError> {
        self.bump(id, Counter::Reopens).await
    }

    pub async fn record_routing(
        &self,
        id: i64,
        jev_model: Option<&str>,
        verdict: Option<&Value>,
        tiers: &Value,
        reasons: &[String],
    ) -> Result<(), StoreError> {
        let write = Write::Routing {
            jev_model,
            verdict,
            tiers,
            reasons,
        };
        self.write_with_events(id, write, &[]).await.map(|_| ())
    }

    /// `(jev_model, verdict, tiers, reasons)` of every routing decision, oldest first.
    pub async fn routing_decisions(
        &self,
        id: i64,
    ) -> Result<Vec<(Option<String>, Option<Value>, Value, Vec<String>)>, StoreError> {
        let rows = sqlx::query(
            "SELECT jev_model, verdict_json, tiers_json, reasons FROM routing_decisions WHERE task_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let verdict: Option<String> = r.get("verdict_json");
                let tiers: String = r.get("tiers_json");
                let reasons: String = r.get("reasons");
                Ok((
                    r.get("jev_model"),
                    verdict
                        .map(|v| {
                            serde_json::from_str(&v).map_err(|_| StoreError::Corrupt(v.clone()))
                        })
                        .transpose()?,
                    serde_json::from_str(&tiers).map_err(|_| StoreError::Corrupt(tiers.clone()))?,
                    reasons.lines().map(str::to_string).collect(),
                ))
            })
            .collect()
    }

    pub async fn record_stage_run(&self, run: &StageRunRecord) -> Result<(), StoreError> {
        self.write_with_events(run.task_id, Write::StageRun(run), &[])
            .await
            .map(|_| ())
    }

    pub async fn stage_runs(&self, id: i64) -> Result<Vec<StageRunRecord>, StoreError> {
        let rows = sqlx::query("SELECT * FROM stage_runs WHERE task_id = ? ORDER BY id")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| StageRunRecord {
                task_id: r.get("task_id"),
                stage: r.get("stage"),
                model_id: r.get("model_id"),
                exit: r.get("exit"),
                turns: r.get::<i64, _>("turns") as u32,
                input_tokens: r.get::<i64, _>("input_tokens") as u64,
                output_tokens: r.get::<i64, _>("output_tokens") as u64,
                session_dir: PathBuf::from(r.get::<String, _>("session_dir")),
                gate_score: r.get("gate_score"),
                started_at: r.get("started_at"),
                finished_at: r.get("finished_at"),
                cache_read_tokens: r.get::<i64, _>("cache_read_tokens") as u64,
                cache_write_tokens: r.get::<i64, _>("cache_write_tokens") as u64,
                actual_model: r.get("actual_model"),
                cost_usd: r.get("cost_usd"),
                quota_units: r.get("quota_units"),
            })
            .collect())
    }

    /// Timestamp (as GitHub reports it) of the last comment given to the reply check.
    pub async fn last_reply_seen(&self, id: i64) -> Result<Option<String>, StoreError> {
        Ok(
            sqlx::query("SELECT last_comment_at FROM replies_seen WHERE task_id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .map(|r| r.get("last_comment_at")),
        )
    }

    pub async fn set_last_reply_seen(&self, id: i64, at: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO replies_seen (task_id, last_comment_at) VALUES (?, ?)
             ON CONFLICT (task_id) DO UPDATE SET last_comment_at = excluded.last_comment_at",
        )
        .bind(id)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_escalated(&self, id: i64, escalated: bool) -> Result<(), StoreError> {
        self.update(
            id,
            "UPDATE tasks SET escalated = ?, updated_at = ? WHERE id = ?",
            |q| q.bind(escalated as i64),
        )
        .await
    }

    /// Clears the attempt, review-round and escalation counters: the task starts over
    /// (a reply answered Provefab's question, or `provefab add` requeued it).
    pub async fn reset_counters(&self, id: i64) -> Result<(), StoreError> {
        self.update(
            id,
            "UPDATE tasks SET attempts = 0, review_rounds = 0, escalated = 0, updated_at = ? WHERE id = ?",
            |q| q,
        )
        .await
    }

    /// Keeps a structured stage answer or a text Provefab wrote (`plan`, `review`, `question`).
    pub async fn record_output(
        &self,
        id: i64,
        kind: &str,
        value: &Value,
    ) -> Result<(), StoreError> {
        self.write_with_events(id, Write::Output { kind, value }, &[])
            .await
            .map(|_| ())
    }

    /// Runs `write` and appends `events` in one `BEGIN IMMEDIATE`
    /// transaction (spec section 4): the record never disagrees with the
    /// tables the pipeline reads. Returns the new event ids, in order.
    pub async fn write_with_events(
        &self,
        task_id: i64,
        write: Write<'_>,
        events: &[Event],
    ) -> Result<Vec<i64>, StoreError> {
        self.write_with_inference(task_id, write, events, None)
            .await
    }

    /// `write_with_events` plus, in the same transaction and after the
    /// events, the inferences of `rule` for the findings of `pass`. Each
    /// (finding, rule, rule version) is inferred at most once (spec section 4).
    pub async fn write_with_inference(
        &self,
        task_id: i64,
        write: Write<'_>,
        events: &[Event],
        infer: Option<(Rule, u32)>,
    ) -> Result<Vec<i64>, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        exec_write(&mut tx, task_id, &write).await?;
        let mut ids = Vec::with_capacity(events.len());
        for e in events {
            ids.push(append_event(&mut tx, task_id, e).await?);
        }
        if let Some((rule, pass)) = infer {
            infer_findings(&mut tx, task_id, rule, pass).await?;
        }
        tx.commit().await?;
        Ok(ids)
    }

    /// Records a review output, its `review` event and its findings with
    /// new keys, in one transaction (spec sections 3.2 and 4).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_review(
        &self,
        task_id: i64,
        value: &Value,
        reviewer_model: &str,
        pass: u32,
        round: u32,
        verdict: &str,
        findings: &[crate::stage::Finding],
    ) -> Result<Vec<String>, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        exec_write(
            &mut tx,
            task_id,
            &Write::Output {
                kind: "review",
                value,
            },
        )
        .await?;
        let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM findings WHERE task_id = ?")
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?;
        let keys: Vec<String> = (0..findings.len())
            .map(|i| format!("F{}", taken + 1 + i as i64))
            .collect();
        let event_id = append_event(
            &mut tx,
            task_id,
            &Event::Review {
                reviewer_model: reviewer_model.into(),
                pass,
                round,
                verdict: verdict.into(),
                findings: keys.clone(),
            },
        )
        .await?;
        for (k, f) in keys.iter().zip(findings) {
            sqlx::query(
                "INSERT INTO findings (task_id, key, pass, round, reviewer_model, severity, file, line, text, event_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(task_id)
            .bind(k)
            .bind(pass)
            .bind(round)
            .bind(reviewer_model)
            .bind(match f.severity { crate::stage::Severity::Blocking => "blocking", crate::stage::Severity::Minor => "minor" })
            .bind(&f.file)
            .bind(f.line)
            .bind(&f.text)
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(keys)
    }

    pub async fn findings(&self, task_id: i64) -> Result<Vec<FindingRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM findings WHERE task_id = ? ORDER BY id")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| FindingRow {
                id: r.get("id"),
                task_id: r.get("task_id"),
                key: r.get("key"),
                pass: r.get::<i64, _>("pass") as u32,
                round: r.get::<i64, _>("round") as u32,
                reviewer_model: r.get("reviewer_model"),
                severity: r.get("severity"),
                file: r.get("file"),
                line: r.get::<Option<i64>, _>("line").map(|l| l as u32),
                text: r.get("text"),
                event_id: r.get("event_id"),
            })
            .collect())
    }

    pub async fn events(&self, task_id: i64) -> Result<Vec<StoredEvent>, StoreError> {
        let rows = sqlx::query("SELECT * FROM change_events WHERE task_id = ? ORDER BY seq")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(stored_event).collect()
    }

    /// Worker runs (stage runs with a model, not gates) started at or after `since`,
    /// across all tasks (the daily budget, D53).
    pub async fn worker_runs_since(&self, since: i64) -> Result<u32, StoreError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM stage_runs WHERE model_id != '' AND started_at >= ?",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("n") as u32)
    }

    /// How many outputs of `kind` the task has.
    pub async fn count_outputs(&self, id: i64, kind: &str) -> Result<u32, StoreError> {
        let row =
            sqlx::query("SELECT COUNT(*) AS n FROM stage_outputs WHERE task_id = ? AND kind = ?")
                .bind(id)
                .bind(kind)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.get::<i64, _>("n") as u32)
    }

    /// The newest `n` outputs of `kind`, oldest first.
    pub async fn recent_outputs(
        &self,
        id: i64,
        kind: &str,
        n: u32,
    ) -> Result<Vec<Value>, StoreError> {
        let rows = sqlx::query(
            "SELECT json FROM stage_outputs WHERE task_id = ? AND kind = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(id)
        .bind(kind)
        .bind(i64::from(n))
        .fetch_all(&self.pool)
        .await?;
        let mut out = rows
            .iter()
            .map(|r| {
                let s: String = r.get("json");
                serde_json::from_str(&s).map_err(|_| StoreError::Corrupt(s))
            })
            .collect::<Result<Vec<Value>, _>>()?;
        out.reverse();
        Ok(out)
    }

    /// Every stored `pending_github` effect across all tasks, oldest first:
    /// `(row id, task id, effect json)`.
    pub async fn pending_github(&self) -> Result<Vec<(i64, i64, Value)>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, task_id, json FROM stage_outputs WHERE kind = 'pending_github' ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let s: String = r.get("json");
                let json = serde_json::from_str(&s).map_err(|_| StoreError::Corrupt(s))?;
                Ok((r.get("id"), r.get("task_id"), json))
            })
            .collect()
    }

    /// Deletes one stage-output row (a retried `pending_github` effect that succeeded).
    pub async fn delete_output(&self, row_id: i64) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM stage_outputs WHERE id = ?")
            .bind(row_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The newest output of `kind`, if any.
    pub async fn last_output(&self, id: i64, kind: &str) -> Result<Option<Value>, StoreError> {
        let row = sqlx::query(
            "SELECT json FROM stage_outputs WHERE task_id = ? AND kind = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .bind(kind)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let s: String = r.get("json");
            serde_json::from_str(&s).map_err(|_| StoreError::Corrupt(s))
        })
        .transpose()
    }

    /// Runs an `UPDATE ... updated_at = ? WHERE id = ?` whose leading parameters `bind` supplies.
    async fn update(
        &self,
        id: i64,
        sql: &'static str,
        bind: impl FnOnce(
            sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
        )
            -> sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    ) -> Result<(), StoreError> {
        let done = bind(sqlx::query(sql))
            .bind(now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(StoreError::UnknownTask(id));
        }
        Ok(())
    }

    async fn bump(&self, id: i64, counter: Counter) -> Result<u32, StoreError> {
        let sql = match counter {
            Counter::Attempts => {
                "UPDATE tasks SET attempts = attempts + 1, updated_at = ? WHERE id = ? RETURNING attempts"
            }
            Counter::ReviewRounds => {
                "UPDATE tasks SET review_rounds = review_rounds + 1, updated_at = ? WHERE id = ? RETURNING review_rounds"
            }
            Counter::Reopens => {
                "UPDATE tasks SET reopen_count = reopen_count + 1, updated_at = ? WHERE id = ? RETURNING reopen_count"
            }
        };
        let row = sqlx::query(sql)
            .bind(now())
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::UnknownTask(id))?;
        Ok(row.get::<i64, _>(0) as u32)
    }
}

#[derive(Clone, Copy)]
enum Counter {
    Attempts,
    ReviewRounds,
    Reopens,
}

fn parse_state(s: &str) -> Result<TaskState, StoreError> {
    TaskState::parse(s).ok_or_else(|| StoreError::Corrupt(s.to_string()))
}

fn post_merge_check_row(r: &SqliteRow) -> Result<PostMergeCheckRow, StoreError> {
    let state: String = r.get("state");
    let kind: Option<String> = r.get("failure_kind");
    let failed_commands = match r.get::<Option<String>, _>("failed_commands") {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        None => Vec::new(),
    };
    let flaky = match r.get::<Option<String>, _>("flaky") {
        Some(s) => serde_json::from_str(&s).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        None => Vec::new(),
    };
    Ok(PostMergeCheckRow {
        id: r.get("id"),
        task_id: r.get("task_id"),
        merge_sha: r.get("merge_sha"),
        base: r.get("base"),
        pr_url: r.get("pr_url"),
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

fn task_row(r: &SqliteRow) -> Result<TaskRow, StoreError> {
    let kind: Option<String> = r.get("kind");
    let worktree: Option<String> = r.get("worktree");
    Ok(TaskRow {
        id: r.get("id"),
        repo: r.get("repo"),
        issue_number: r.get::<i64, _>("issue_number") as u64,
        issue_url: r.get("issue_url"),
        title: r.get("title"),
        author: r.get("author"),
        state: parse_state(&r.get::<String, _>("state"))?,
        kind: kind
            .map(|k| TaskKind::parse(&k).ok_or(StoreError::Corrupt(k)))
            .transpose()?,
        attempts: r.get::<i64, _>("attempts") as u32,
        review_rounds: r.get::<i64, _>("review_rounds") as u32,
        branch: r.get("branch"),
        worktree: worktree.map(PathBuf::from),
        pr_url: r.get("pr_url"),
        pr_state: r.get("pr_state"),
        reopen_count: r.get::<i64, _>("reopen_count") as u32,
        escalated: r.get::<i64, _>("escalated") != 0,
    })
}

/// One existing write, run in the same transaction as its record events.
#[derive(Debug)]
pub enum Write<'a> {
    Nothing,
    StageRun(&'a StageRunRecord),
    Output {
        kind: &'a str,
        value: &'a Value,
    },
    Routing {
        jev_model: Option<&'a str>,
        verdict: Option<&'a Value>,
        tiers: &'a Value,
        reasons: &'a [String],
    },
    SetPr {
        url: &'a str,
        state: &'a str,
    },
    SetPrState(&'a str),
}

async fn exec_write(
    conn: &mut SqliteConnection,
    task_id: i64,
    write: &Write<'_>,
) -> Result<(), StoreError> {
    match write {
        Write::Nothing => {}
        Write::StageRun(run) => {
            if run.task_id != task_id {
                return Err(StoreError::Corrupt(format!(
                    "stage run for task {} written under task {task_id}",
                    run.task_id
                )));
            }
            sqlx::query(
                "INSERT INTO stage_runs (task_id, stage, model_id, exit, turns, input_tokens, output_tokens, session_dir, gate_score, started_at, finished_at,
                                         cache_read_tokens, cache_write_tokens, actual_model, cost_usd, quota_units)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(run.task_id)
            .bind(&run.stage)
            .bind(&run.model_id)
            .bind(&run.exit)
            .bind(run.turns as i64)
            .bind(run.input_tokens as i64)
            .bind(run.output_tokens as i64)
            .bind(run.session_dir.display().to_string())
            .bind(&run.gate_score)
            .bind(run.started_at)
            .bind(run.finished_at)
            .bind(run.cache_read_tokens as i64)
            .bind(run.cache_write_tokens as i64)
            .bind(&run.actual_model)
            .bind(run.cost_usd)
            .bind(run.quota_units)
            .execute(&mut *conn)
            .await?;
        }
        Write::Output { kind, value } => {
            sqlx::query("INSERT INTO stage_outputs (task_id, kind, json, at) VALUES (?, ?, ?, ?)")
                .bind(task_id)
                .bind(*kind)
                .bind(value.to_string())
                .bind(now())
                .execute(&mut *conn)
                .await?;
        }
        Write::Routing {
            jev_model,
            verdict,
            tiers,
            reasons,
        } => {
            sqlx::query(
                "INSERT INTO routing_decisions (task_id, jev_model, verdict_json, tiers_json, reasons, at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(task_id)
            .bind(*jev_model)
            .bind(verdict.map(Value::to_string))
            .bind(tiers.to_string())
            .bind(reasons.join("\n"))
            .bind(now())
            .execute(&mut *conn)
            .await?;
        }
        Write::SetPr { url, state } => {
            let done = sqlx::query(
                "UPDATE tasks SET pr_url = ?, pr_state = ?, updated_at = ? WHERE id = ?",
            )
            .bind(*url)
            .bind(*state)
            .bind(now())
            .bind(task_id)
            .execute(&mut *conn)
            .await?;
            if done.rows_affected() == 0 {
                return Err(StoreError::UnknownTask(task_id));
            }
        }
        Write::SetPrState(state) => {
            let done = sqlx::query("UPDATE tasks SET pr_state = ?, updated_at = ? WHERE id = ?")
                .bind(*state)
                .bind(now())
                .bind(task_id)
                .execute(&mut *conn)
                .await?;
            if done.rows_affected() == 0 {
                return Err(StoreError::UnknownTask(task_id));
            }
        }
    }
    Ok(())
}

async fn infer_findings(
    conn: &mut SqliteConnection,
    task_id: i64,
    rule: Rule,
    pass: u32,
) -> Result<(), StoreError> {
    let kind = Event::FindingInferred {
        finding: String::new(),
        rule,
        rule_version: RULE_VERSION,
    }
    .kind();
    let keys: Vec<String> = match rule {
        Rule::UnaddressedAtMerge => {
            sqlx::query_scalar(
                "SELECT key FROM findings f WHERE task_id = ? AND pass = ? AND NOT EXISTS (\
                   SELECT 1 FROM change_events e WHERE e.task_id = f.task_id AND e.kind = 'finding_disposition' \
                   AND json_extract(e.payload, '$.finding') = f.key) ORDER BY id",
            )
            .bind(task_id)
            .bind(pass)
            .fetch_all(&mut *conn)
            .await?
        }
        _ => {
            sqlx::query_scalar("SELECT key FROM findings WHERE task_id = ? AND pass = ? ORDER BY id")
                .bind(task_id)
                .bind(pass)
                .fetch_all(&mut *conn)
                .await?
        }
    };
    for key in keys {
        let seen: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM change_events WHERE task_id = ? AND kind = ? \
             AND json_extract(payload, '$.finding') = ? AND json_extract(payload, '$.rule_version') = ?",
        )
        .bind(task_id)
        .bind(kind)
        .bind(&key)
        .bind(RULE_VERSION)
        .fetch_optional(&mut *conn)
        .await?;
        if seen.is_none() {
            append_event(
                conn,
                task_id,
                &Event::FindingInferred {
                    finding: key,
                    rule,
                    rule_version: RULE_VERSION,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn append_event(
    conn: &mut SqliteConnection,
    task_id: i64,
    e: &Event,
) -> Result<i64, StoreError> {
    let payload = serde_json::to_string(e).map_err(|x| StoreError::Corrupt(x.to_string()))?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO change_events (task_id, seq, kind, source, schema_version, payload, at) \
         VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM change_events WHERE task_id = ?), ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(task_id)
    .bind(task_id)
    .bind(e.kind())
    .bind(e.source().as_str())
    .bind(e.schema_version())
    .bind(payload)
    .bind(now())
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

fn stored_event(r: &SqliteRow) -> Result<StoredEvent, StoreError> {
    let payload: String = r.get("payload");
    Ok(StoredEvent {
        id: r.get("id"),
        task_id: r.get("task_id"),
        seq: r.get("seq"),
        kind: r.get("kind"),
        source: r.get("source"),
        schema_version: r.get::<i64, _>("schema_version") as u32,
        payload: serde_json::from_str(&payload).map_err(|x| StoreError::Corrupt(x.to_string()))?,
        at: r.get("at"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applied migrations are checksummed by sqlx: editing one, even a
    /// comment, makes every existing database refuse to open. These are the
    /// checksums installed databases carry (seen live when the Provefab rename
    /// touched a comment, plan 5 task 8).
    #[test]
    fn migrations_are_frozen() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let sums: Vec<String> = sqlx::migrate!("./migrations")
            .iter()
            .map(|m| hex(&m.checksum))
            .collect();
        assert_eq!(
            sums,
            [
                "58482ad7ee578abba1976c7bc8f21ed4c3c2a481c6a24f74d64164e7f37c01e6dc1931ad68e527a67df118bf2983cdbd",
                "fa9d5e7daa5ddeec2a821c7123b4fcca83a08f59d147187aac8a58405466bc12e383f228b0a8574c97fa57817f6fa432",
                "9f9cca5cfdefacd436e685a2daf6b99f3a4d7104dbda6fd6c5df338adc59f1379ec0d15f86887d96c4ec23b3c1e3eaa5",
                "daca2e3a57485b3a91ce46779913c341fec2ab5c757e523088b9c89a9fe3683a62f86b83b3adf5775babc4a8fb17ac49",
                "5f568cb3d17aaf248e9f208ef39268d14393dd87157e348b5946549ebf4a359aedf16a05374529315b4dba8d461e9832",
            ]
        );
    }
    use serde_json::json;

    fn issue(n: u64) -> NewIssue {
        NewIssue {
            repo: "o/r".into(),
            number: n,
            url: format!("https://github.com/o/r/issues/{n}"),
            title: format!("Issue {n}"),
            author: "alice".into(),
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("provefab.db")).await.unwrap();
        (dir, s)
    }

    use crate::record::{Event, MergedBy};

    #[tokio::test]
    async fn events_share_the_write_transaction_and_seq_is_gap_free() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        let ids = s
            .write_with_events(
                id,
                Write::SetPr {
                    url: "u",
                    state: "open",
                },
                &[Event::PrOpened {
                    url: "u".into(),
                    head: None,
                    base: "main".into(),
                    pass: 1,
                }],
            )
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
        s.write_with_events(
            id,
            Write::Nothing,
            &[
                Event::IssueReopened { previous_pass: 1 },
                Event::Merged {
                    sha: None,
                    base: None,
                    by: MergedBy::Human,
                    pass: 1,
                },
            ],
        )
        .await
        .unwrap();
        let ev = s.events(id).await.unwrap();
        assert_eq!(ev.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(ev[0].kind, "pr_opened");
        assert_eq!(ev[0].source, "fact");
        assert_eq!(
            s.task(id).await.unwrap().unwrap().pr_url.as_deref(),
            Some("u")
        );
    }

    #[tokio::test]
    async fn a_failing_write_rolls_its_events_back() {
        let (_d, s) = store().await;
        // Unknown task: the UPDATE matches nothing, so the write fails.
        let r = s
            .write_with_events(
                999,
                Write::SetPr {
                    url: "u",
                    state: "open",
                },
                &[Event::IssueReopened { previous_pass: 1 }],
            )
            .await;
        assert!(matches!(r, Err(StoreError::UnknownTask(999))));
        assert!(s.events(999).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn existing_writers_still_write_without_events() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        s.record_output(id, "plan", &json!({"a": 1})).await.unwrap();
        s.set_pr(id, "u", "open").await.unwrap();
        assert!(s.events(id).await.unwrap().is_empty());
        assert_eq!(
            s.last_output(id, "plan").await.unwrap(),
            Some(json!({"a": 1}))
        );
    }

    #[tokio::test]
    async fn intake_is_idempotent_and_logs_the_first_transition() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        assert_eq!(s.add_issue(&issue(1)).await.unwrap(), None);
        let t = s.task(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Queued);
        assert_eq!(t.issue_number, 1);
        let log = s.transitions(id).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!((log[0].from, log[0].to), (None, TaskState::Queued));
    }

    #[tokio::test]
    async fn transitions_are_logged_in_order_with_their_reasons() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(2)).await.unwrap().unwrap();
        s.transition(id, TaskState::Classified, "jev: bugfix, difficulty 1.2")
            .await
            .unwrap();
        s.transition(id, TaskState::Planning, "plan on sonnet")
            .await
            .unwrap();
        let log = s.transitions(id).await.unwrap();
        let steps: Vec<_> = log
            .iter()
            .map(|t| (t.from, t.to, t.reason.as_str()))
            .collect();
        assert_eq!(
            steps,
            vec![
                (None, TaskState::Queued, "intake"),
                (
                    Some(TaskState::Queued),
                    TaskState::Classified,
                    "jev: bugfix, difficulty 1.2"
                ),
                (
                    Some(TaskState::Classified),
                    TaskState::Planning,
                    "plan on sonnet"
                ),
            ]
        );
        assert!(matches!(
            s.transition(999, TaskState::Failed, "x").await,
            Err(StoreError::UnknownTask(999))
        ));
    }

    #[tokio::test]
    async fn state_survives_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provefab.db");
        let id = {
            let s = Store::open(&path).await.unwrap();
            let id = s.add_issue(&issue(3)).await.unwrap().unwrap();
            s.transition(id, TaskState::Implementing, "resume me")
                .await
                .unwrap();
            s.set_worktree(id, "provefab/3-fix", Path::new("/w/3"))
                .await
                .unwrap();
            id
        };
        let s = Store::open(&path).await.unwrap();
        let t = s.task(id).await.unwrap().unwrap();
        assert_eq!(t.state, TaskState::Implementing);
        assert_eq!(t.branch.as_deref(), Some("provefab/3-fix"));
        assert_eq!(t.worktree, Some(PathBuf::from("/w/3")));
    }

    #[tokio::test]
    async fn counters_kind_pr_links_and_filters() {
        let (_d, s) = store().await;
        let a = s.add_issue(&issue(4)).await.unwrap().unwrap();
        let b = s.add_issue(&issue(5)).await.unwrap().unwrap();
        s.set_kind(a, TaskKind::Bugfix).await.unwrap();
        assert_eq!(s.bump_attempts(a).await.unwrap(), 1);
        assert_eq!(s.bump_attempts(a).await.unwrap(), 2);
        s.reset_attempts(a).await.unwrap();
        assert_eq!(s.bump_review_rounds(a).await.unwrap(), 1);
        s.set_pr(a, "https://github.com/o/r/pull/9", "open")
            .await
            .unwrap();
        s.set_pr_state(a, "merged").await.unwrap();
        assert_eq!(s.record_reopen(a).await.unwrap(), 1);
        s.transition(b, TaskState::NeedsInfo, "underspecified 0.91")
            .await
            .unwrap();
        let t = s.task(a).await.unwrap().unwrap();
        assert_eq!(t.kind, Some(TaskKind::Bugfix));
        assert_eq!((t.attempts, t.review_rounds, t.reopen_count), (0, 1, 1));
        assert_eq!(t.pr_url.as_deref(), Some("https://github.com/o/r/pull/9"));
        assert_eq!(t.pr_state.as_deref(), Some("merged"));
        let waiting = s.tasks_in(&[TaskState::NeedsInfo]).await.unwrap();
        assert_eq!(waiting.iter().map(|t| t.id).collect::<Vec<_>>(), vec![b]);
        assert_eq!(s.task_by_url(&issue(5).url).await.unwrap().unwrap().id, b);
        assert!(matches!(
            s.set_kind(42, TaskKind::Docs).await,
            Err(StoreError::UnknownTask(42))
        ));
    }

    #[tokio::test]
    async fn routing_stage_runs_and_replies_round_trip() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(6)).await.unwrap().unwrap();
        s.record_routing(
            id,
            Some("jev-1.13.0"),
            Some(&json!({"difficulty": 2.1})),
            &json!({"plan": "frontier", "implement": "standard", "review": "frontier"}),
            &["difficulty 2.10 -> implement Standard".to_string()],
        )
        .await
        .unwrap();
        let r = s.routing_decisions(id).await.unwrap();
        assert_eq!(r[0].0.as_deref(), Some("jev-1.13.0"));
        assert_eq!(r[0].1, Some(json!({"difficulty": 2.1})));
        assert_eq!(r[0].3, vec!["difficulty 2.10 -> implement Standard"]);
        let run = StageRunRecord {
            task_id: id,
            stage: "implement".into(),
            model_id: "codex-hi".into(),
            exit: "completed".into(),
            turns: 7,
            input_tokens: 1000,
            output_tokens: 200,
            session_dir: "/s/6/implement".into(),
            gate_score: Some("0,0,0".into()),
            started_at: 10,
            finished_at: 20,
            cache_read_tokens: 3000,
            cache_write_tokens: 400,
            actual_model: Some("claude-sonnet-5-5".into()),
            cost_usd: Some(0.0123),
            quota_units: None,
        };
        s.record_stage_run(&run).await.unwrap();
        assert_eq!(s.stage_runs(id).await.unwrap(), vec![run]);
        assert_eq!(s.last_reply_seen(id).await.unwrap(), None);
        s.set_last_reply_seen(id, "2026-09-24T10:00:00Z")
            .await
            .unwrap();
        s.set_last_reply_seen(id, "2026-09-24T11:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            s.last_reply_seen(id).await.unwrap().as_deref(),
            Some("2026-09-24T11:00:00Z")
        );
    }

    #[tokio::test]
    async fn a_stage_run_written_under_another_task_is_refused() {
        let (_d, s) = store().await;
        let a = s.add_issue(&issue(7)).await.unwrap().unwrap();
        let b = s.add_issue(&issue(8)).await.unwrap().unwrap();
        let run = StageRunRecord {
            task_id: a,
            stage: "plan".into(),
            model_id: "m".into(),
            exit: "completed".into(),
            turns: 1,
            input_tokens: 0,
            output_tokens: 0,
            session_dir: "/s".into(),
            gate_score: None,
            started_at: 1,
            finished_at: 2,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            actual_model: None,
            cost_usd: None,
            quota_units: None,
        };
        let err = s
            .write_with_events(b, Write::StageRun(&run), &[])
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
        assert!(s.stage_runs(a).await.unwrap().is_empty());
        assert!(s.stage_runs(b).await.unwrap().is_empty());
    }

    /// Final review C1: `--workers N` moves tasks concurrently through one store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn review_c1_concurrent_transitions_all_succeed() {
        let (_d, s) = store().await;
        let s = std::sync::Arc::new(s);
        let mut ids = Vec::new();
        for n in 0..8 {
            ids.push(s.add_issue(&issue(100 + n)).await.unwrap().unwrap());
        }
        let mut handles = Vec::new();
        for i in 0..200 {
            let s = s.clone();
            let id = ids[i % ids.len()];
            let to = if i % 2 == 0 {
                TaskState::Planning
            } else {
                TaskState::Implementing
            };
            handles.push(tokio::spawn(async move {
                s.transition(id, to, "concurrent").await
            }));
        }
        let mut failures = 0;
        for h in handles {
            if h.await.unwrap().is_err() {
                failures += 1;
            }
        }
        assert_eq!(failures, 0, "transitions failed under concurrency");
        let logged: usize = {
            let mut n = 0;
            for id in &ids {
                n += s.transitions(*id).await.unwrap().len() - 1;
            }
            n
        };
        assert_eq!(logged, 200);
    }

    /// Final review (minor, promoted): the same issue under another spelling of its URL
    /// (case, trailing slash) or repo slug is the same task.
    #[tokio::test]
    async fn review_same_issue_different_url_is_not_a_second_task() {
        let (_d, s) = store().await;
        s.add_issue(&issue(7)).await.unwrap().unwrap();
        let mut variant = issue(7);
        variant.url = "https://github.com/O/R/issues/7/".into();
        variant.repo = "O/R".into();
        assert_eq!(s.add_issue(&variant).await.unwrap(), None);
    }

    #[test]
    fn rfc3339_matches_github_timestamps() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_790_316_980), "2026-09-25T06:16:20Z");
    }

    #[tokio::test]
    async fn review_i1_i2_transition_and_writes_everything_together() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(4)).await.unwrap().unwrap();
        s.bump_attempts(id).await.unwrap();
        s.set_escalated(id, true).await.unwrap();
        s.transition_and(id, TaskState::Implementing, "changes", Also::NewRound)
            .await
            .unwrap();
        let t = s.task(id).await.unwrap().unwrap();
        assert_eq!(
            (t.state, t.attempts, t.escalated, t.review_rounds),
            (TaskState::Implementing, 0, false, 1)
        );
        let q = json!({"text": "why?"});
        s.transition_and(
            id,
            TaskState::NeedsInfo,
            "ask",
            Also::Question {
                question: &q,
                seen_at: "2026-09-25T10:00:00Z",
            },
        )
        .await
        .unwrap();
        assert_eq!(s.last_output(id, "question").await.unwrap(), Some(q));
        assert_eq!(
            s.last_reply_seen(id).await.unwrap().as_deref(),
            Some("2026-09-25T10:00:00Z")
        );
    }

    #[tokio::test]
    async fn outputs_and_ladder_state_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provefab.db");
        let id = {
            let s = Store::open(&path).await.unwrap();
            let id = s.add_issue(&issue(3)).await.unwrap().unwrap();
            s.record_output(id, "plan", &json!({"v": 1})).await.unwrap();
            s.record_output(id, "plan", &json!({"v": 2})).await.unwrap();
            s.set_escalated(id, true).await.unwrap();
            s.bump_attempts(id).await.unwrap();
            s.bump_review_rounds(id).await.unwrap();
            id
        };
        let s = Store::open(&path).await.unwrap();
        assert_eq!(
            s.last_output(id, "plan").await.unwrap(),
            Some(json!({"v": 2}))
        );
        assert_eq!(s.last_output(id, "review").await.unwrap(), None);
        let t = s.task(id).await.unwrap().unwrap();
        assert!(t.escalated && t.attempts == 1 && t.review_rounds == 1);
        s.reset_counters(id).await.unwrap();
        let t = s.task(id).await.unwrap().unwrap();
        assert!(!t.escalated && t.attempts == 0 && t.review_rounds == 0);
    }

    async fn merged_task(s: &Store) -> i64 {
        s.add_issue(&issue(1)).await.unwrap().unwrap()
    }

    fn new_check(task_id: i64, sha: &str) -> NewPostMergeCheck<'_> {
        NewPostMergeCheck {
            task_id,
            merge_sha: sha,
            base: "main",
            pr_url: Some("https://github.com/o/r/pull/8"),
            commit_count: Some(1),
            auto_merged: false,
        }
    }

    #[tokio::test]
    async fn post_merge_check_creation_is_idempotent() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let a = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        let b = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.state, CheckState::Queued);
        assert_eq!(a.commit_count, Some(1));
        assert_eq!(a.pr_url.as_deref(), Some("https://github.com/o/r/pull/8"));
        assert_eq!(s.post_merge_checks(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn advance_is_compare_and_set_and_patches_only_given_columns() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        // Wrong `from`: nothing written.
        assert!(
            !s.advance_post_merge(
                c.id,
                CheckState::Verifying,
                CheckState::Passed,
                &CheckPatch::default()
            )
            .await
            .unwrap()
        );
        assert_eq!(s.post_merge_infra_error(c.id).await.unwrap(), 1);
        let patch = CheckPatch {
            base_sha: Some("base1".into()),
            failure_summary: Some("é".repeat(3000)),
            bump_base_moves: true,
            ..Default::default()
        };
        assert!(
            s.advance_post_merge(
                c.id,
                CheckState::Queued,
                CheckState::VerificationFailed,
                &patch
            )
            .await
            .unwrap()
        );
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert_eq!(c.state, CheckState::VerificationFailed);
        assert_eq!(c.base_sha.as_deref(), Some("base1"));
        assert_eq!(
            c.failure_summary.as_ref().unwrap().chars().count(),
            crate::post_merge::SUMMARY_MAX
        );
        assert_eq!((c.base_moves, c.infra_errors), (1, 0));
        assert!(c.started_at.is_some() && c.finished_at.is_none());
        // `None` keeps `base_sha`; a terminal state stamps `finished_at`.
        assert!(
            s.advance_post_merge(
                c.id,
                CheckState::VerificationFailed,
                CheckState::Blocked,
                &CheckPatch::default()
            )
            .await
            .unwrap()
        );
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert_eq!(c.base_sha.as_deref(), Some("base1"));
        assert!(c.finished_at.is_some());
    }

    #[tokio::test]
    async fn an_unknown_post_merge_state_is_corruption_not_a_default() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        sqlx::query("UPDATE post_merge_checks SET state = 'failed' WHERE id = ?")
            .bind(c.id)
            .execute(&s.pool)
            .await
            .unwrap();
        assert!(matches!(
            s.post_merge_check_by_id(c.id).await,
            Err(StoreError::Corrupt(_))
        ));
    }

    #[tokio::test]
    async fn notification_targets_are_tracked_separately() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let c = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        s.mark_post_merge_notified(c.id, NoticeTarget::Issue)
            .await
            .unwrap();
        let c = s.post_merge_check_by_id(c.id).await.unwrap().unwrap();
        assert!(c.issue_notified_at.is_some() && c.pr_notified_at.is_none());
    }

    #[tokio::test]
    async fn open_post_merge_work_is_listed_whatever_the_task_state() {
        let (_d, s) = store().await;
        let id = merged_task(&s).await;
        let other = s.add_issue(&issue(2)).await.unwrap().unwrap();
        assert!(s.tasks_with_open_post_merge().await.unwrap().is_empty());
        let a = s
            .ensure_post_merge_check(&new_check(id, "abc"))
            .await
            .unwrap();
        let b = s
            .ensure_post_merge_check(&new_check(other, "def"))
            .await
            .unwrap();
        assert_eq!(s.tasks_with_open_post_merge().await.unwrap(), [id, other]);
        // A pass has nothing to announce: done.
        s.advance_post_merge(
            a.id,
            CheckState::Queued,
            CheckState::Passed,
            &CheckPatch::default(),
        )
        .await
        .unwrap();
        // A blocked check stays listed until both targets are notified.
        s.advance_post_merge(
            b.id,
            CheckState::Queued,
            CheckState::Blocked,
            &CheckPatch::default(),
        )
        .await
        .unwrap();
        assert_eq!(s.tasks_with_open_post_merge().await.unwrap(), [other]);
        s.mark_post_merge_notified(b.id, NoticeTarget::Issue)
            .await
            .unwrap();
        assert_eq!(s.tasks_with_open_post_merge().await.unwrap(), [other]);
        s.mark_post_merge_notified(b.id, NoticeTarget::Pr)
            .await
            .unwrap();
        assert!(s.tasks_with_open_post_merge().await.unwrap().is_empty());
    }
}
