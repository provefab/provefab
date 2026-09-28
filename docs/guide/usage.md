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
- **routing:** Jev's verdict (kind, difficulty and confidence, scope, vagueness) and the tiers chosen.
- **stage runs:** every stage, with its model, outcome, turns, tokens, check score and session directory (full transcript).
- **last plan / review / failure:** the latest structured answers.

## Trying it safely

`provefab run --dry-run` classifies and routes every open labelled issue and prints one line per issue. It runs no agent, posts nothing and changes no label. Use it to tune Jev's thresholds or to check a new repository.
