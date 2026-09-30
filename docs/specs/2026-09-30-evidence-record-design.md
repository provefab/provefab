# Evidence and decision record

- Date: 2026-09-30
- Status: approved in conversation; awaiting written-spec review
- Feature: #2 of the product direction (handoff `docs/handoff-2026-09-29-feature1-post-merge.md`). Foundation for #3 (reviewer calibration) and #5 (risk policy).

## 1. Intent

Keep a durable, append-only record of every change Provefab makes: what was observed, what models claimed, what humans decided, and what Provefab inferred, each labelled as such. v1 is a machine foundation (owner decision 1): it captures early the data reviewer calibration will need, and exposes it through `provefab log` and a JSON Lines export. It builds no report UI.

The record never calls anything proof. Checks that pass are "checks passed", not "correct".

## 2. What exists today (2026-09-30)

- `transitions`, `routing_decisions`, `stage_runs` (model, tokens, cost, session dir, gate score), `stage_outputs` (free JSON by kind; every review is appended, `recent_outputs` reads them all), `post_merge_checks`.
- A review is `{verdict, findings[{file, line, severity, text}]}`. Findings have no stable identity.
- Nothing records what a human did with a finding.
- Pro's second reviewer runs through the core `review()` stage (Pro only sets `approvals_needed`), so core capture covers it.
- The PR body mixes facts (reproduction, gates) and model claims (plan, review notes).

## 3. Data model

Migration `0005_record.sql` (one migration; published migrations are never edited).

### 3.1 `change_events` (append-only)

| Column | Meaning |
|---|---|
| `id` | primary key |
| `task_id` | the change |
| `seq` | 1, 2, 3... per task, gap-free, `UNIQUE(task_id, seq)` |
| `kind` | event type (section 3.3) |
| `source` | `fact`, `claim`, `human`, `inferred` |
| `schema_version` | payload version for this `kind` |
| `payload` | JSON, defined by a typed Rust enum |
| `at` | unix seconds |

Rows are never updated. They are deleted only by `provefab prune` (section 6).

### 3.2 `findings`

| Column | Meaning |
|---|---|
| `id` | primary key |
| `task_id` | the change |
| `key` | `F1`, `F2`... unique per task, never reused, `UNIQUE(task_id, key)` |
| `round` | review round that raised it |
| `reviewer_model` | model id of the reviewer |
| `severity` | `blocking` or `minor` |
| `file`, `line` | location (line nullable) |
| `text` | the finding as the model wrote it |
| `event_id` | the `review` event that raised it |

Each review round creates its own findings. No fuzzy matching across rounds. A finding's current disposition is the latest `finding_disposition` event for it; with none, it is `open`.

### 3.3 Event kinds (v1, schema_version 1)

fact (observed by Provefab):

- `routed`: tier, verdict source (Jev or fallback).
- `stage_run`: stage, configured model id, actual model, provider, input/output/cache tokens, cost, exit.
- `gates_run`: stage/round, per command `{command, exit, timed_out, passed, output_ref}`. `output_ref` is the local session path, never the output.
- `reproduction`: command, `failed_before_fix` (bool).
- `pr_opened`: url, head sha, base.
- `merged`: sha (or null), base, `by` (`auto` or `human`).
- `post_merge`: check id, final state, failure kind.
- `issue_reopened`.

claim (stated by a model; never shown as evidence):

- `plan`: summary, steps, risks.
- `review`: reviewer model, round, verdict, finding keys.

human:

- `finding_disposition`: finding key, disposition (`accepted`, `rejected`, `fixed`, `waived`), reason (optional), GitHub login, association, comment URL.

inferred (each names its rule and rule version):

- `finding_unaddressed_at_merge`: the PR merged while the finding had no human disposition.
- `finding_followed_by_revert`: the finding's PR got a post-merge revert PR (`post_merge` reached `revert_open`).
- `finding_followed_by_reopen`: the finding's issue was reopened after the merge.

## 4. Capture

- Every existing write point emits its event in the same SQLite transaction as its current write: routing, `record_stage_run`, gates, reproduction, plan and review outputs, PR opening, `record_merge`, post-merge terminal transitions, reopen. The record cannot diverge from the existing tables: if either write fails, both roll back.
- Existing tables stay the source of the pipeline's decisions. In v1 the record drives nothing.
- Finding keys are assigned in the same transaction as the `review` event.
- Inferences run when their trigger event is written (`merged`, a `post_merge` reaching `revert_open`, `issue_reopened`). Each is idempotent: at most one event per finding, rule and rule version.

## 5. Human dispositions

- The PR body's "Review notes" shows each finding with its key, for example ``F3 · blocking · `src/lib.rs:42` · text``, followed by one help line: "Reply `/provefab F3 rejected` (or accepted, fixed, waived), optionally followed by a reason."
- Commands are read from PR comments. While the PR is open, `watch_pr` already reads them every tick. After the merge, PR comments are read at most hourly with the existing reopen watch, until the task is archived (two weeks). That is one more `gh` call per hour per merged task.
- Grammar: one or more commands per comment, each `/provefab <key> <disposition>[: reason]` on its own line. Keys are case-insensitive.
- Only the issue author and people with write access count (the existing authorization rule). Others are ignored and logged locally.
- Idempotent per comment URL. A later disposition for the same finding becomes current; all stay in the log.
- `/provefab` comments are excluded from the human findings the pipeline reads from a closed PR, so a command never becomes a change request.
- No automatic reply. An unknown key or an invalid disposition is ignored, logged locally and shown by `provefab log`.
- Reactions are not used in v1: findings live in the PR body, and a reaction on the body cannot name a finding. Re-open: commands turn out rarely used.

## 6. CLI

- `provefab log <id>`: a "record" section with the event timeline, each line tagged `[fact]`, `[claim]`, `[human]` or `[inferred]`, then the findings with their current disposition.
- `provefab export [--repo <slug>] [--since <date>] [--with-text]`: JSON Lines, one line per event and one per finding with its current disposition.
  - Default: identities, states, verdicts, severities, dispositions, costs, exit codes, SHAs. Free text (plan, finding text, human reasons) is replaced by its length and SHA-256.
  - `--with-text` includes the free text. Command output is never exported; it stays in local session directories.
- `provefab prune --before <date> [--yes]`: deletes events and findings of finished tasks last updated before the date. Finished means state `failed`, or state `pr_open` with `pr_state` `done` or `archived`; any other task is kept whatever its age, and so is any task with a non-terminal post-merge check. Without `--yes` it prints what it would delete and deletes nothing.
- Retention: kept locally without limit (owner decision 3). The database is the user's.

## 7. Schema evolution

- Each `kind` carries its own `schema_version`. Readers accept every known version and report an unknown one without failing.
- New kinds or versions never rewrite old rows.
- Wording rule in `log` and export: never "proof", never "correct"; inferences carry their rule name.

## 8. Tests

- Each capture point emits exactly one event in the same transaction; a failing record write rolls back the main write, and the reverse.
- `seq` is gap-free per task; finding keys are stable and never reused across rounds.
- Commands: authorized and unauthorized authors; several commands in one comment; replaying the same comment (no duplicate); unknown key; a command on a closed PR is not a human finding; a command after the merge.
- Inferences: idempotent on replay; rule name and version recorded.
- Export: without `--with-text` no free text leaves (a sentinel secret in a finding and a plan never appears); every line is valid JSON.
- Prune: without `--yes` nothing is deleted.
- Existing pipeline, post-merge and CLI tests stay green.
- A real run on `antoinehoriot/factory-sandbox` before release: a PR with findings, a `/provefab` command, a merge, and `provefab export` output checked.

## 9. Budget and scope

- Exactly one migration (`0005_record.sql`) and one new module (`record.rs`). No new `Hub` method if the PR comments already read suffice. A second module, or a new `Hub` method, is a STOP and a question to the owner.

Out of v1, each with a re-open trigger:

- Human-readable report (Markdown/HTML): a customer asks for an audit trail.
- Reactions for dispositions: commands turn out rarely used.
- Inferring "fixed" from later rounds: explicit dispositions turn out too rare.
- Backfilling tasks from before the migration: never in v1; the record starts at the migration.

## 10. Decisions

1. v1 is a machine foundation, exposed by `log` and export only (owner, 2026-09-30). Why: capture calibration data early without building UI.
2. Explicit human dispositions come first; inferences are stored separately and labelled (owner). Why: calibrating on inference alone would be biased.
3. Local retention without limit, redacted export by default, explicit prune (owner). Why: calibration needs history; the database belongs to the user.
4. Append-only event log plus a typed `findings` table (owner, approach A). Why: replayable history for #3, and addressable findings for commands and queries.
5. Events are written in the same transaction as the existing writes. Why: the record can never disagree with what the pipeline did.
6. No inference of "fixed" in v1. Why: "the next round did not report it" cannot tell a fixed finding from a wrong one.
