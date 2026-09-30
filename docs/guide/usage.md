# Usage

## The life of an issue

1. **You set the `provefab` label** on an open issue. It is the only authorization Provefab needs. Only people who can triage the repository can set a label.
2. **Classification.** Jev reads the title, body and labels. It estimates the kind (bugfix, feature, refactor, docs, test, chore), the difficulty, the scope, and whether the issue is too vague.
3. **A question, if the issue is vague.** Provefab asks in a comment and sets `provefab:needs-info`. Answer with a comment, as the issue author or a collaborator. If the answer is enough, the task resumes, and your answer is added to the issue text every stage reads.
4. **Plan** (read-only tools). For a bugfix, the plan must give a **reproduction command** that fails before the fix. If it already passes, Provefab stops: the bug is not reproduced.
5. **Implementation.** The agent changes the code in an isolated worktree. The guard filters every tool call.
6. **Checks.** Your `gates` commands run, plus the reproduction command for a bugfix. On failure, Provefab:
   - triages the cause with Jev;
   - retries;
   - moves to a stronger model;
   - stops if nothing works.
7. **Review** by a different model provider than the implementer's. If it asks for changes, the implementer gets all of them, earlier rounds included, and must write a test for each.
8. **Pull request.** Provefab commits, pushes and opens the PR. Its text lists the checks, the routing, and any deleted or disabled tests. It comments on the issue and sets `provefab:in-pr`. The PR then waits for your review.
9. **Optional post-merge verification.** If you set `post_merge_checks`, Provefab runs them on the exact commit its PR produced on the base branch. A failing command is rerun once on a fresh checkout; if it passes then, the run counts as passed and the command is reported as flaky. On a confirmed failure, Provefab runs the same checks on the current base: if they pass, a later commit already fixed it and no revert is proposed. Otherwise Provefab prepares a revert on the current base and opens a revert PR only if the reverted tree passes the same checks. Every merge is verified. A revert is never proposed for a multi-commit PR a person merged by squash or rebase (Provefab cannot tell the two apart); a merge commit is reverted as one commit. In that case its failure is reported and a human decides. Revert conflicts, failing revert checks, and a base branch that keeps moving also require a human. Provefab never merges a revert PR. These are repository commands, not production monitoring.

## Risk-aware changes

After each implementation round is committed, Provefab classifies the changed files by path (see `[repos.risk]` in [Configuration](configuration.md#reposrisk-risk-aware-policy)). Nothing changes when no category is detected. When one is:

- **Extra checks.** The `checks` of the detected categories run after your `gates`, as a second pass named `risk-gates`. A failure counts like a failing gate: the task goes back to implementation. The PR's Checks section lists them after your gates, each marked `(risk: <category>)`.
- **Frontier reviewer from another provider.** The review runs on the `frontier` tier when a detected category asks for it (the default; `reviewer_tier = "standard"` opts a category out) and your catalog has a frontier model from another provider than the model that implemented the round. Without one, the review keeps its usual reviewer from another provider (a standard model, as a rule), the task goes on, and the Risk section says `reviewer: standard (no frontier reviewer from another provider is configured)`. The `reviewer:` value is always the tier of the model that actually reviewed: when another rule already asked for a frontier review (a high review risk from Jev, a second approver), the review can run on your frontier model from the implementer's provider, and the line then says `reviewer: frontier (no frontier reviewer from another provider is configured)`. `provefab doctor` warns about this. To get frontier reviews, add a frontier model on a second provider.
- **Risk section.** The PR body lists each category with its paths (five, then "and N more"), the checks it added and the reviewer tier that the rule above chose.
- **Issue labels.** `<label>:risk-<category>` (for example `provefab:risk-migrations`) goes on the issue once the round's changed files are classified. These labels are created at startup, and again if missing. A later round that no longer touches a category removes its label.
- **`unknown`.** If the changed files cannot be computed, the round is classified `unknown`: it gets a frontier reviewer by the same rule as a frontier category, and the round continues.
- **Provefab Pro.** Provefab Pro does not auto-merge a risky change unless you allow its category. Its first review follows the rule above. Its second review is on the frontier tier and avoids the first approver's provider when it can: with a single frontier model, both approvals can come from that same model, and Pro then refuses the auto-merge with its own "same model" reason, even when you allow the category. Configure frontier models on two providers to allow it.

`provefab log <id>` shows a `risk_classified` line for each round: the categories and how many paths matched each, or `none`.

## Writing issues that land

What works best:

- **One issue, one change.** "Add `min()` next to `max()`, with a test" lands in a minute. "Redo the stats module" takes several passes.
- **State the expected behaviour, not only the problem.** For a bug: what happens, what should happen, and a precise failing example, such as `mean(&[f64::MAX, f64::MAX])` returning `inf`.
- **List the edge cases you know**: empty input, infinities, very large values, Unicode... Otherwise the reviewer finds them one by one, and each costs a round.
- **Name the files or functions involved** when you know them. The plan gets shorter and more accurate.
- **Say what must not change** (public API, output format) when it matters.

## Answering Provefab

| Situation | What to do |
|---|---|
| `provefab:needs-info` and a question in a comment | Answer in a comment (you or a collaborator). Provefab checks the answer with Jev and resumes. Without a Jev key, restart by hand with `provefab add <url>`. |
| PR open and it suits you | Merge it. Provefab notices, labels the issue `provefab:merged` and cleans the worktree. |
| PR open and it does not suit you | **Close it with a comment** saying what to change. The comment becomes a blocking finding, and Provefab starts again on a fresh branch (`...-r2`). Review comments and inline comments count too. |
| PR open and you no longer want the work | **Close it without a comment.** Provefab stops (`provefab:failed`). |
| Issue reopened after a merge (the bug is back) | Nothing: Provefab starts a new pass by itself. |
| `provefab:failed` | Read its comment and `provefab log <id>`. Fix the cause (vague issue, environment...), then `provefab add <url>`. |

`provefab add <url>` on a stopped task starts a **new pass** on a fresh branch from the base. Provefab keeps every blocking finding from earlier passes and puts the `provefab` label back.

## Recording decisions on review findings

Each finding in the PR's **Review notes** has a key (`F1`, `F2`...). To record what you decided about one, comment on the PR:

```
/provefab F1 rejected: the null case cannot happen here
/provefab F2 fixed
```

- Syntax: `/provefab F<n> accepted|rejected|fixed|waived`, then an optional reason after a colon or a space. One command per line, several per comment. Keys and dispositions are case-insensitive. Lines inside code fences are ignored.
- Only the issue author, and repository owners, organization members and collaborators, count. Other commands, unknown keys and invalid dispositions are ignored, and `provefab log` shows them.
- Commands are read while the PR is open, when it is closed, before a merge is recorded, and every hour after the merge until the task is archived (two weeks). Provefab never replies.
- A later command for the same finding replaces the earlier one as current. All stay in the record.
- Each comment counts once per finding: repeating the same key in one comment records it once, and editing a comment keeps the disposition first recorded from it. To change your decision, post a new comment.
- On a PR closed with a comment, command lines are left out of your change request. The rest of the comment still counts.

`provefab log <id>` includes a **record** section (after the post-merge checks, before the last plan, review and failure outputs): a numbered timeline, each line tagged `[fact]` (observed by Provefab), `[claim]` (stated by a model), `[human]` (your commands) or `[inferred]` (with its rule name and version), then the findings with their current disposition (`open` until you decide).

`provefab export [--repo <owner/name>] [--since YYYY-MM-DD] [--with-text]` prints the record as JSON Lines, one line per event and one per finding. `--since` keeps events from that date and the findings of reviews from that date; `--repo` matches any case. By default free text (plans, finding text, your reasons, ignored command lines, reproduction and gate commands, error exits) is replaced by its length and SHA-256, and an event this version does not know exports `{"unknown": true}` instead of its payload. `--with-text` includes it. File paths (finding locations, risk-classified paths) and model names are exported as is. Command output is never exported.

`provefab prune --before YYYY-MM-DD [--yes]` deletes the record of finished tasks last updated before that date. Without `--yes` it lists the tasks and deletes nothing.

## Task states

| State | Meaning | Next |
|---|---|---|
| `queued` | queued | Provefab takes it as soon as a worker is free |
| `classified`, `planning`, `implementing`, `gating`, `reviewing` | in progress | nothing to do |
| `needs_info` | question asked | answer in a comment |
| `waiting` | waiting: every model busy or rate-limited, a transient failure, or the daily budget reached | resumes by itself; the reason is in `provefab status` |
| `pr_open` | PR open (or merged: see `provefab log`) | review, merge or close |
| `needs_you` | Provefab needs you (broken environment, pass budget spent, reproduction impossible...) | read the comment, fix, `provefab add` |
| `failed` | Provefab gave up, or you closed the PR without a comment | `provefab add` to start again |

## Reading `provefab log <id>`

- **transitions:** every state change, with its reason. For a tool error, the full detail is here and never on GitHub.
- **routing:** Jev's verdict (kind, difficulty and confidence, scope, vagueness, planning depth, review risk), the tiers chosen, and why, for example `review_risk 0.80 < 1.5 -> review Standard`.
- **routes:** for each stage, the model it ran on and why, for example `review -> sonnet-sub: subscription, quota weight 1.0 (prefer subscription)`.
- **stage runs:** every stage, with its model, outcome, turns, tokens (`in/out`, then `(+cache read/write)` when the model cached), cost, check score and session directory (full transcript). The cost is in dollars for a model signed in by API key (`cost $0.0123`) and in quota units for a subscription (`quota 0.42`: millions of tokens times the model's quota weight). A `total:` line sums the task.
The pull request's **Routing** section lists the model of each stage and ends with the task's cost, for example `Cost: $0.4210 API · 1.20 quota units.` A part that is zero is left out.
- **post-merge checks:** state, failure kind, merged SHA, base tip, revert commit, revert PR link, failed commands and flaky commands. Session directories for `post-merge`, `post-merge-base` (the same checks on the current base tip) and `revert-check` in **stage runs** contain command results.
- **last plan / review / failure:** the latest structured answers.

## Measuring

`provefab stats` sums up each repository: how many issues became pull requests, how many were merged (automatically or by a person), closed or reopened after a merge, post-merge check outcomes and revert PRs, the median time from issue to pull request, and which reviewers approved the merged ones. Automatic merges are counted from the version that introduced the command on.

## Trying it safely

`provefab run --dry-run` classifies and routes every open labelled issue and prints one line per issue. It runs no agent, posts nothing and changes no label. Use it to tune Jev's thresholds or to check a new repository.
