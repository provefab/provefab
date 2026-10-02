# Reviewing human pull requests

- Date: 2026-10-02
- Status: approved design (owner, 2026-10-02); implemented in 0.5.0, amended 2026-10-02 (see the notes marked "amended")
- Feature: on request, Provefab reviews a pull request written by a person with a reviewer from its catalog, the repository rules and the risk policy, posts one summary comment, and records the findings in the evidence record like its own pull requests.

## 1. Intent

Teams already write most pull requests by hand. Reviewing them with the same reviewer, rules and record Provefab uses for its own pull requests is a lighter way to start than an agent that writes code, and every decision a person records on those findings feeds calibration and rule proposals.

## 2. Owner decisions

1. Trigger: a label `<label>:review` on the pull request, or a `/provefab review` comment from an authorised person. Never automatic for every pull request.
2. Output: one summary comment per pull request, updated on each review; no inline comments, no GitHub review state, no commit status.
3. Edition: free in the core; Provefab Pro's existing features (second reviewer, calibration, rule proposals, cost reports) apply to these reviews too.
4. Architecture: a pull-request review is a task of a new mode, `pr_review` (approach 1 of 3).

## 3. Model

- Migration `0008`: `tasks.mode TEXT NOT NULL DEFAULT 'issue'` (`issue` or `pr_review`) and `tasks.pr_head TEXT` (the commit last reviewed); the table is rebuilt so that uniqueness is `UNIQUE (repo, mode, issue_number)` instead of `UNIQUE (repo, issue_number)`, keeping every `id` and every row that references a task. Why: in a Jira or Linear repository, pull request #12 and ticket ENG-12 share the number 12. The rebuild follows SQLite's documented procedure for changing a table's constraints with foreign keys enforced; the plan verifies how to run it under sqlx migrations and a test migrates a database holding tasks, findings, events, stage runs and outputs.
- For a `pr_review` task, `issue_number` is the pull request number, `issue_url` its URL, `title` its title, `author` its author, `issue_key` NULL.
- (amended 2026-10-02 in the plan) The migration runs with `-- no-transaction` and its own `BEGIN IMMEDIATE`/`COMMIT` around SQLite's procedure (plan decision 1).
- (amended 2026-10-02 during implementation) The migration switches `PRAGMA foreign_keys` off and back on itself, is safe to run twice, and carries no in-file `foreign_key_check`: ids are copied verbatim.

## 4. Trigger and intake

- Label: `<label>:review` (for example `provefab:review`), created at startup like the other labels, on an open pull request whose base is the repository's `base`. Putting a label on a pull request requires triage or write access, so the label is the authorisation.
- Command: a comment whose line is exactly `/provefab review` (case-insensitive, outside code fences) on an open pull request, from an `OWNER`, `MEMBER` or `COLLABORATOR`. The pull request's author alone is not enough: on a public repository a contributor from a fork could otherwise spend the owner's model subscriptions.
- Excluded: pull requests whose head branch starts with `provefab/` (Provefab's own), closed or merged pull requests, and pull requests to another base.
- Intake: the existing poll also lists open pull requests with the label (one new `Forge` method) and reads new `/provefab review` commands on open pull requests already known or labelled; a new trigger on a pull request with a `pr_review` task starts a new review round on the current head; the first trigger creates the task.
- A push to a reviewed pull request does not start a review by itself; the label being removed and added again, or a new command, does.
- (amended 2026-10-02 in the plan) The one new `Forge` method lists every open pull request into the base with its labels and comments, so a command works on any open pull request (decision 2). A label removed and added again is seen between two polls (decision 4). The label is created on GitHub-tracker repositories only; on Jira and Linear repositories it is created by hand, and the comment works there too (decision 5). Triggers act between rounds (decision 8).
- (amended 2026-10-02 during implementation) Provefab's own comments never trigger. A flip of the label during a running round is not seen. Old member commands present at upgrade can queue one review.

## 5. Review run

- The pull request's head is fetched with `refs/pull/<n>/head` (forks included) into a read-only worktree; the diff is taken from the merge base with `origin/<base>` to the head, as GitHub shows it.
- Risk: the changed files are classified with the repository's risk policy (feature #5). Rules: selected for the changed files (repository rules feature), read from the base commit.
- Reviewer tier: standard; frontier when a detected risk category asks for it and a frontier model is configured. There is no implementer, so no provider is avoided. Routing as for stages (subscription first, then API keys by price); the cost is recorded on the task and counts in the daily stage budget.
- Prompt: a variant of the review prompt in which the pull request's title and description replace the issue (both untrusted data), the out-of-scope rule becomes "the change does not do what the description says, or does more", and the review rubric and repository rules apply unchanged.
- Approvals: when the review policy asks for two approvals (Provefab Pro's second reviewer), the second review runs on a model of another provider than the first, as for Provefab's own pull requests.
- Findings get keys `F<n>` continuing across rounds and are recorded like other reviews (`review` event, `findings` rows with the reviewer model and the cited rule).
- (amended 2026-10-02 in the plan) Only classification: nothing from the pull request runs, since it may come from a fork (decision 6). Two reviews whatever the first verdict when two approvals are asked (decision 9). The tier rule is as written (decision 10). Findings are redacted before they are kept (decision 11). Rules are read from the base, pinned each round (decision 16).
- (amended 2026-10-02 during implementation) The head is fetched with `--no-tags --no-recurse-submodules`; no gates, no risk checks and no risk labels apply; finding text and git or `gh` error text are redacted.
- (amended 2026-10-02 after final review) The reviewer runs under a review marker the guard reads: every write is refused, and every shell command but a read-only allowlist (`cat`, `head`, `tail`, `ls`, `wc`, `grep`, `rg`, `find` without `-exec`, `-execdir`, `-ok`, `-delete` or `-fprint*`, and `git show`, `diff`, `log`, `status`, `blame`, `ls-files`, `grep`, `rev-parse`, `cat-file`), piped only into each other, with no redirect to a file. Issue tasks keep their tools.
- (amended 2026-10-02 after final review) Under the review marker every read stays inside the worktree, for every tool and every path argument of the allowed commands: absolute paths outside it, `~`, `$` expansions, `..` and symbolic links that lead out are refused; a Codex command whose directory the guard cannot see must start with `cd <a directory in the worktree> &&`; Codex's web search and image viewer are off.
- (amended 2026-10-02 after final review) A review's transient retries are counted per round, not per pass.
- (amended 2026-10-02 after final review) The reviewers of a round are derived from its `review` events, so a restart after a review was recorded never pays for another.

## 6. The summary comment

- One comment per pull request, starting with the bot line, created on the first review and edited on later ones (found by a hidden marker `<!-- provefab-pr-review -->`).
- Content: the verdict (`no blocking finding` or `N blocking findings`), the findings in the review-notes format (`F1 · R3 · blocking · src/a.rs:12 · text`), `Rules: R1, R3`, the risk categories detected, the reviewer model(s) and the commit reviewed, and how to record a decision (`/provefab F1 rejected: reason`).
- Provefab never sets a GitHub review state, never approves, never requests changes, never merges a person's pull request.
- (amended 2026-10-02 in the plan) The comment is edited by the id recorded when it was posted, through `pr_comment`'s new `edit` argument, not found by the marker; it is posted anew when deleted (decision 3).
- (amended 2026-10-02 during implementation) A not-found error that is not permanent counts as deleted; when GitHub returns no id, the next round posts a new comment.

## 7. After the review

- The task stays watched while the pull request is open: `/provefab F<n>` commands are read as today; on merge, the `unaddressed_at_merge` inference applies to the last round's findings; on close, the task ends.
- Post-merge checks and reverts stay limited to Provefab's own pull requests.
- (amended 2026-10-02 in the plan) A review task never writes to the tracker (decision 7). End states: merged and closed (decision 12). Authorisation of the two commands is as in section 4 (decision 13).
- (amended 2026-10-02 during implementation) Reviews parked in `needs_you` or `failed` are watched for their end only; `F<n>` decisions on a parked review are read at merge or close, or at the next round. An ended review keeps its state and is prunable whatever its state.

## 8. Visibility

- `provefab status`: `o/r PR #57 · reviewed (2 blocking)`; `log` and `export` carry `mode`; `stats` counts pull-request reviews apart from issue tasks.
- (amended 2026-10-02 in the plan) Formats (decision 18): `status` shows `reviewed (N blocking)`, `merged` or `closed`; `log` shows `mode pr_review`; every `export` line carries `mode`; `stats` has one line `pull request reviews: N pull requests · N rounds · N findings (N blocking)`.

## 9. Documentation and landing

- Core: new `docs/guide/pr-review.md`; `usage.md` (label, command), `security.md` (public repositories, forks, who can trigger), README (labels table, one day-to-day row).
- Landing: one Free line, one FAQ entry. Terms: unchanged (free feature).
- No em-dashes; no claim that a review guarantees correct code.

## 10. Tests

- Migration 0008 on a database with tasks, findings, events, stage runs and outputs: ids and links kept; pull request #12 and ticket ENG-12 coexist; issue tasks unchanged.
- Intake: label; authorised command; author-only command refused; Provefab's own branches excluded; other base excluded; a second trigger starts a new round.
- Pipeline: fetch of a fork head; risk and rules selected; reviewer tier; comment created then edited; no approval, no merge; second reviewer when the policy asks for two; findings recorded and commands applied; merge inference.
- Non-regression: issue tasks unchanged (prompts, PR bodies, labels, record).
- Real run: on the sandbox, a hand-written pull request reviewed via the label and via the command.

## 11. Budget and scope

- Core: at most one new module (`pr_review.rs`), one migration (`0008`), one new `Forge` method, no new dependency. More is a STOP.
- Pro: only what is needed for its existing features to cover these reviews (a cost line); no new feature.
- Version 0.5.0.

Out of v1, each with a re-open trigger:

- Inline comments on the diff: teams ask for them.
- A blocking commit status: a team wants reviews to gate merges.
- Reviewing every pull request automatically: a pilot asks for it.
- GitLab merge requests: with a GitLab forge.

## 12. Decisions

1-4: owner decisions in section 2.
5. Rebuild `tasks` for `UNIQUE (repo, mode, issue_number)` (controller). Why: pull request and ticket numbers collide in Jira and Linear repositories. Cost if wrong: a delicate migration; covered by a migration test on real-shaped data.
6. On a pull request, the author alone cannot trigger a review (controller). Why: fork contributors on public repositories would spend the owner's subscriptions. Cost: an outside contributor asks a maintainer.
7. A push does not re-trigger (controller). Why: cost control and an explicit request each time. Re-open: teams ask for review on every push of a labelled pull request.
