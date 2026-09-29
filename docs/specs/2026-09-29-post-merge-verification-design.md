# Post-merge verification and safe rollback

- Date: 2026-09-29 (revision 2, same day)
- Status: revision 2 approved in conversation; awaiting written-spec review. Release v0.1.1 unpublished.
- Scope: Provefab-created pull requests only
- Supersedes: revision 1 of this file (single `process_post_merge` function with implicit states). Revision 1 decisions in section 13 still hold unless amended there.

## 1. Intent

After a Provefab-created pull request is merged, Provefab verifies the resulting base-branch commit with repository-defined commands. A confirmed failure is recorded with evidence, reported on GitHub, and, when the current base is still broken and a safe rollback exists, produces a revert pull request. Provefab never merges the revert.

This validates configured repository commands against the merged tree. It is not deployment or production health monitoring, and public copy must not claim otherwise.

## 2. v1 boundaries

Included:

- GitHub repositories configured in `[[repos]]` with a non-empty `post_merge_checks`.
- PRs whose task is known locally and whose PR was opened by Provefab.
- Squash merges, single-commit PRs merged any way, and merge commits (see section 6).
- Durable, per-merge check state in SQLite; command output local only.
- Human-reviewed revert PRs.

Not included (each with its re-open trigger):

- Monitoring arbitrary PRs.
- Deployment probes, health metrics, automatic rollback.
- Auto-merging or force-pushing a revert.
- Agent diagnosis or fixes of the failure.
- `provefab verify <id>` manual rerun. Re-open: a user asks for a manual rerun.
- A separate post-merge timeout. Re-open: `gate_timeout` proves wrong for post-merge commands.
- Reverting multi-commit PRs merged by rebase. Re-open: a pilot repository merges by rebase.

## 3. Configuration

```toml
[[repos]]
slug = "owner/repo"
base = "main"
gates = ["cargo fmt -- --check", "cargo test"]
post_merge_checks = ["cargo test", "cargo clippy --all-targets -- -D warnings"]
```

- Empty by default; empty disables the feature for the repo. `doctor` reports it. It never reuses `gates` implicitly.
- Empty command strings are rejected by validation.
- Commands run from the repo root through the existing gate runner, with the same credential scrubbing, and `limits.gate_timeout`.
- Command strings may appear in GitHub comments. The docs must say: never put secrets on the command line; use the environment.

## 4. Merge detection and attribution

On observing a known task PR as merged (poll path or Provefab's own auto-merge path), `record_merge`:

1. Reads from GitHub: `mergeCommit.oid`, `baseRefName`, `commits` count, and whether Provefab auto-merged it (existing `auto_merged` output).
2. If checks are configured and `mergeCommit` or `baseRefName` is missing, keeps the PR watched and retries on the next poll, up to **1 hour** after first seeing it merged. After that, it records the merge normally and creates the check directly in `blocked` with `failure_kind = attribution_missing`.
3. Never infers the base. The auto-merge path must not fall back to `repo.base` when GitHub omits `baseRefName`; that is `attribution_missing`.
4. If `baseRefName != repo.base`, no check is created.
5. Otherwise inserts the check row (`UNIQUE(task_id, merge_sha)`, idempotent) in `queued`, storing `commit_count` and `auto_merged`. The merge-strategy decision (section 6) needs the parent count of `merge_sha`, so it happens at the `queued` tick after a fetch, not in `record_merge`.
6. Only merges observed after opt-in create rows. Enabling the feature never checks old merges.

A task qualifies only when: it exists locally, its stored PR URL is the merged PR, the PR was created by Provefab's recorded `open_pr`, and the merge SHA and base come from GitHub. If any is uncertain: `blocked`, no command runs, no revert.

## 5. State machine

One row per merged commit. Each scheduler tick performs **at most one transition** for a row, under the per-repo lock shared with normal worktree operations. The new state and every value it depends on are written **before** any external side effect. Every non-terminal state is safe to replay in full after a crash.

| State | Tick action | Next |
|---|---|---|
| `queued` | Fetch. Decide merge strategy (section 6). | `verifying`, or `blocked` (`unsafe_merge_strategy`) |
| `verifying` | Remove `<id>-verify` if present, create a fresh detached worktree at `merge_sha`, run all checks. Rerun each failing or timed-out command **once** in a new fresh worktree. | all pass first time: `passed`; pass after rerun: `passed` with `flaky` set; any command fails twice: `verification_failed` |
| `verification_failed` | Fetch. `merge_sha` must be an ancestor of the base tip, else `blocked` (`base_diverged`). Record `base_sha` = base tip. Run checks in a fresh detached worktree `<id>-base` at `base_sha` (same one-rerun rule). | base passes: `superseded`; base fails: `preparing_revert` |
| `preparing_revert` | Create a fresh detached worktree `<id>-revert` at `base_sha`. Apply the revert (section 6). Run checks on the reverted tree (one-rerun rule). Require a clean worktree afterwards. Record `revert_sha` = HEAD and point local branch `provefab/revert-<id>-<base_moves>` at it (`revert_branch`). | `revert_ready`; or `blocked` (`revert_conflict`, `revert_checks_failed`, `dirty_tree`) |
| `revert_ready` | Fetch. If the base tip is not `base_sha`: increment `base_moves`, go to `verification_failed`; on the 3rd move, `blocked` (`base_moved`). Else push `revert_sha` to `revert_branch`. If the remote branch already exists it must equal `revert_sha`, else `blocked` (`branch_conflict`). Create the PR; a reused PR must be same-repo, target `repo.base`, and have head `revert_sha`, else `blocked` (`branch_conflict`). Record `revert_pr_url`. | `revert_open` |
| `passed`, `superseded`, `revert_open`, `blocked` | Terminal. Only pending notifications (section 7) and cleanup (section 9). | |

Rules:

- `verifying`, `verification_failed`, `preparing_revert` have no external side effects: replay is harmless.
- `revert_ready` side effects are idempotent because the exact `revert_sha` is recorded first and verified on the remote branch and the PR head. The branch is never the source of truth.
- The branch name derives from the check id and the attempt (`base_moves`), so it never collides with task branches, another check, or a branch pushed by an earlier attempt before the base moved (which would otherwise read as `branch_conflict`).
- The revert PR is not a task: it never triggers post-merge verification itself.
- The scheduler advances post-merge rows on **every** tick, before the hourly throttle it applies to merged tasks' reopen watch (`scheduler.rs`); otherwise a failure would take one hour per transition.
- A command failure is never retried beyond the single rerun. Infra errors follow section 8.

## 6. Merge strategies and revert construction

Decided at `queued` from git (parent count of `merge_sha`) and the row's `commit_count` / `auto_merged`:

| Case | Revert |
|---|---|
| `merge_sha` has 2 parents (merge commit) | `git revert --no-edit -m 1 <merge_sha>` |
| 1 parent, `commit_count == 1` | `git revert --no-edit <merge_sha>` |
| 1 parent, `commit_count > 1`, `auto_merged` by Provefab (always squash) | `git revert --no-edit <merge_sha>` |
| 1 parent, `commit_count > 1`, merged by a human | `blocked` (`unsafe_merge_strategy`): squash and rebase cannot be told apart, and reverting the last rebased commit alone would be partial |
| more than 2 parents or missing `commit_count` | `blocked` (`unsafe_merge_strategy`) |

Reverts run with Provefab's git identity and hooks disabled. A conflict is never resolved automatically. `git revert` commits by itself; Provefab never calls `commit_all` on a revert worktree.

## 7. Notifications and published text

- Emitted when a row reaches `superseded`, `revert_open`, or `blocked`. `passed` is silent (visible in `log` and `stats`).
- Addressed by **check id**, never by "latest check of the task".
- Two targets tracked separately: `issue_notified_at`, `pr_notified_at`.
- Each body ends with the marker `<!-- provefab-post-merge:<check_id> -->`. Before posting to a target, read its remote comments and skip if the marker is present; then set that target's column. Issue success plus PR failure, then retry, yields exactly one comment on each.
- Bodies come only from fixed templates, one per outcome and `failure_kind`: `check_failed` (with `superseded` or revert context), `infra_error`, `revert_conflict`, `revert_checks_failed`, `base_moved`, `base_diverged`, `branch_conflict`, `unsafe_merge_strategy`, `attribution_missing`, `dirty_tree`.
- Allowed variable fields: failing command strings, exit code, timed-out flag, SHAs, PR/issue URLs. Never stderr, command output, error text from git/gh, or local paths. Those stay in local `results.json` and logs.
- `failure_summary` (local) is truncated to 2,000 characters.
- The revert PR body states: original PR and issue, `merge_sha`, `base_sha`, failing commands, that the current base also fails them, that the reverted tree passes them, and `Provefab will not merge this revert automatically.`

## 8. Infra errors

A git/gh error, or an `Err` from the gate runner, leaves the state unchanged, is logged, increments `infra_errors`, and is retried next tick. A successful transition resets `infra_errors` to 0. At 5 consecutive infra errors in the same state: `blocked` (`infra_error`).

## 9. Worktrees and cleanup

- Paths: `~/.provefab/post-merge/<check_id>-verify`, `-verify-rerun`, `-base`, `-revert`. Derived from the id, not stored.
- Every worktree is removed before it is created: residue from a crash is never reused.
- A guard removes the worktree on every exit path of the state handler, including errors. No worktree outlives its state: `revert_ready` pushes `revert_sha` from the main checkout (`git push origin <revert_sha>:refs/heads/provefab/revert-<id>`; worktrees share the object store), so `-revert` is removed at the end of `preparing_revert` too.
- On reaching a terminal state: sweep any `<check_id>-*` directory left behind, run `git worktree prune`, delete the local `revert_branch`. Remote revert branches are never deleted by Provefab.

## 10. Data model

Migration `0004_post_merge.sql` is rewritten (it was never published):

```sql
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
    failed_commands   TEXT,              -- JSON [{command, exit, timed_out}], source of published text
    flaky             TEXT,              -- JSON array of commands saved by a rerun
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

When GitHub never returned the merge SHA (`attribution_missing`), `merge_sha` holds the literal `unknown`; real SHAs are hex, so it cannot collide, and `UNIQUE (task_id, merge_sha)` keeps it single.

`state` values: `queued`, `verifying`, `verification_failed`, `preparing_revert`, `revert_ready`, `passed`, `superseded`, `revert_open`, `blocked`. The Rust side uses an enum with `FromStr`/`as_str`; unknown values are an error, never a default.

Each run of checks writes a `stage_runs` record (stages `post-merge`, `post-merge-base`, `revert-check`) and a local `results.json` with bounded output tails.

## 11. CLI and observability

- `provefab status`: count of checks per state.
- `provefab log <id>`: state, `failure_kind`, `merge_sha`, `base_sha`, `revert_sha`, commands with results, flaky commands, revert PR URL.
- `provefab stats`: passed, flaky, superseded, reverts opened, blocked.

## 12. Tests and validation

Rewrite `crates/provefab/tests/post_merge.rs` with fake `Hub` and `Git` that record calls and order:

- each transition in section 5, and a replay test per non-terminal state;
- one rerun rescues: `passed` + `flaky`; two failures: `verification_failed`; timeout treated as failure;
- green current base: `superseded`, no revert, no push;
- each row of section 6;
- base moves in `revert_ready`: back to `verification_failed`; 3rd move: `blocked`;
- remote branch on another SHA, or reused PR with another head: `blocked`;
- notifications: issue ok, PR error, retry: one comment each; addressed by check id when a task has two checks;
- published bodies contain no stderr or path (a sentinel secret placed in command output never appears in any Hub call);
- missing SHA past 1 hour: `blocked`; missing base on auto-merge: `blocked`;
- 5 infra errors: `blocked`; a success resets the counter;
- no worktree left after any terminal state or error path;
- opt-in does not check old merges;
- existing merged/reopened/archived issue behaviour unchanged.

Checks before completion: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`.

Real run before any tag, in a disposable private GitHub repo:

1. merged PR, green check: `passed`;
2. merged PR, red check: revert PR opened, not merged;
3. fix pushed to base before the tick: `superseded`.

Evidence: `provefab log` output and URLs for each.

## 13. Decisions

Kept from revision 1:

1. `post_merge_checks` is opt-in, empty by default, never reuses `gates`.
2. A revert PR is opened only when the reverted tree passes the configured checks; otherwise `blocked`.
3. Checks run against the exact merge commit GitHub reports on the configured base.

Added in revision 2 (conversation of 2026-09-29):

4. Explicit state machine, one transition per tick, rather than patching the single function. Why: every crash window becomes a named, replayable, testable state. Cost accepted: rewrite of `process_post_merge` and its tests.
5. A failing or timed-out command is rerun once on a fresh checkout before counting as a failure; a rescue is recorded as flaky. Why: one flake must not open a revert PR and erode trust.
6. Before any revert, the checks run on the current base tip; green means `superseded`, no revert. Why: never propose reverting a change whose breakage was already fixed.
7. Merge-strategy rules of section 6. Why: squash is always safe, merge commits need `-m 1`, and only human-merged multi-commit PRs are ambiguous (squash vs rebase). This replaces revision 1's "exactly one commit" rule, which blocked safe squashes.
8. Missing merge SHA: wait at most 1 hour, then `blocked`. Missing base: never inferred. Why: spec rule "captured from GitHub, not inferred".
9. GitHub receives fixed templates only. Why: command output and git errors may contain secrets or local paths.
10. Infra errors: 5 consecutive then `blocked`. Why: bounded retries, no silent infinite loop.

Scope budget: exactly one migration file (0004, rewritten) and no new port trait. A second migration or a new trait is a STOP and a question to the owner.

## 14. Release sequence (unchanged, each step needs the owner's explicit go)

1. Implement this revision; all checks green; real run of section 12.
2. Commit core, tag `v0.1.1`, signed/notarized core ZIP.
3. Pro: depend on tag `v0.1.1`, update `Cargo.lock`, tests/lint/build, rebuild signed/notarized ZIP.
4. Landing: update submodule to the tag, `pnpm test`.
5. GitHub releases, then site deploy.
