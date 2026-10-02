# Pull request reviews

Provefab can review a pull request a person wrote, with a reviewer from your model catalog, your [repository rules](rules.md) and your risk policy. It posts one comment on the pull request and records the findings like those of its own pull requests. It never approves, requests changes on or merges the pull request: a person decides.

## Asking for a review

Either:

- put the `provefab:review` label on the pull request (`<label>:review` with your configured `label`), or
- comment a line that is exactly `/provefab review` (any case, outside a code block).

Provefab reads both at its next poll (every 3 minutes by default). It reviews open pull requests into the repository's `base` branch, never its own (branches starting with `provefab/`). The comment works on Jira and Linear repositories too.

## Who can trigger a review

The label is the authorization, as for issues: only people who can triage the repository can set it. A comment counts only from a repository owner, an organization member or a collaborator. The pull request's author alone cannot ask, so a contributor from a fork cannot spend your model subscriptions. A request from anyone else, and Provefab's own comments, never start a review; `provefab log <id>` records the refused ones as ignored commands.

Read a pull request before you ask for its review: its title, description and diff reach the reviewer as untrusted data (see [Security](security.md)).

A push does not start a review by itself. To review new commits, comment `/provefab review` again, or remove the label and add it back. A label removed and put back between two polls is seen.

Provefab creates the `provefab:review` label at startup when GitHub is the repository's tracker. On a Jira or Linear repository, create it on GitHub once by hand, or use the comment.

On a pull request Provefab has not reviewed yet, it reads the comments GitHub lists with open pull requests, at most 100 per pull request; on a longer discussion, use the label. Once a pull request is reviewed, Provefab reads all its comments.

After an upgrade, a `/provefab review` comment from an authorised person already on an open pull request can queue one review.

## What a review does

1. Provefab fetches the pull request's head (`refs/pull/<n>/head`, so pull requests from forks too) into a worktree of its own, without tags or submodules, and takes the diff from where the pull request branched off `origin/<base>`, as GitHub shows it.
2. It classifies the changed files with your [risk policy](configuration.md#reposrisk-risk-aware-policy) and selects your repository rules for those files, read from the base branch, never from the pull request.
3. A standard-tier reviewer reads the title, the description and the diff; a frontier one when a detected risk category asks for it and a frontier model is configured. When your review policy asks for two approvals (Provefab Pro's second reviewer), both reviews always run, the second on a model from another provider.
4. Provefab records the findings (`F1`, `F2`, ... continuing across reviews) and posts or updates its comment.

Provefab runs no gates, checks or builds on the pull request. The reviewer can only read files and use read-only commands: `cat`, `head`, `tail`, `ls`, `wc`, `grep`, `rg`, `find`, and `git` `show`, `diff`, `log`, `status`, `blame`, `ls-files`, `grep`, `rev-parse` and `cat-file`. Each takes only a short list of options, and any other option is refused: `grep` `-n -i -l -L -c -w -x -F -E -H -h -r -s -q -e`, `rg` `-n -i -l -c -w -x -F -H -s -e --no-heading --hidden`, both `--color=never`; `find` `-name -iname -path -type f|d -maxdepth -mindepth`; `head` and `tail` `-n`; `ls` `-l -a -1 -R -d`; `wc` `-l -w -c`; `cat` none. The guard refuses any other command (scripts, interpreters, build and test tools), any write and any redirect to a file. Claude Code and Pi reviewers have no shell at all. A shell command starts with `cd <worktree> &&` and runs from the worktree root only (`git -C` and moving into a subdirectory are refused, so a committed repository is never entered), and every read stays inside the worktree: the guard refuses paths outside it, `~`, variables and symbolic links that lead out, and it finds no repository a pull request commits (`safe.bareRepository=explicit`). A Codex reviewer has no web search. Provefab only classifies the changed files, so the risk policy's gates and checks do not apply, and it puts no risk label on the pull request. The title and description are data for the reviewer, never instructions. A review task never writes to the issue tracker: its messages go to the pull request.

## Costs and limits

The review's cost is recorded on its task, and each reviewer run counts in the daily budget (`max_stage_runs_per_day`); beyond it the review waits, as other tasks do.

The reviewer sees the diff as far as the prompt's size limit allows; a very large change is cut. Finding text and file names, and the text of a git or GitHub error, have words that look like credentials replaced by `<redacted>` before they are kept or posted.

## The comment

One comment per pull request, edited on each review. Provefab keeps the comment's id when it posts it and edits that comment, whatever it says. If someone deletes it, the next review posts a new one; if GitHub gives no id back, the next review posts a new comment too.

```
*Posted by Provefab (automated), not typed by a person.*

**Provefab review: 1 blocking finding.**

- F1 · R3 · blocking · `src/api/user.rs:12` · unwrap on user input
- F2 · minor · `README.md` · the example uses the old flag

Rules: R1, R3
Risk: none detected
Reviewed by `std-claude` at commit `0123456789ab` (round 1).

To record what you decide on a finding, reply `/provefab F1 rejected: <reason>` (or accepted, fixed, waived).
After a push, comment `/provefab review` for another review. Provefab does not approve, request changes on or merge this pull request.
```

`Rules:` appears when rules were given to the reviewer. The comment lists the findings of the latest review only.

## After the review

- Answer findings with `/provefab F<n> accepted|rejected|fixed|waived`, as on Provefab's own pull requests (see [Usage](usage.md#recording-decisions-on-review-findings)).
- When the pull request merges, the last review's findings nobody answered are recorded as unaddressed at merge. Provefab runs no post-merge check and opens no revert for it.
- A label flipped while a review is running is not seen; ask again once the comment is up.
- When it closes, the review ends. A reopened pull request is reviewed again when asked.
- A review that stopped (`needs_you` or `failed`) is watched only for the pull request merging or closing. Decisions on its findings are read at that point, or at the next review.
- An ended review (merged or closed) keeps its state and `provefab prune` can delete its record.

## Seeing reviews

- `provefab status`: `o/r PR #57 · reviewed (2 blocking)`, then `merged` or `closed`.
- `provefab log <id>` shows `mode pr_review`; every `provefab export` line has a `mode`.
- `provefab stats` counts pull request reviews on their own line, for example `pull request reviews: 3 pull requests · 4 rounds · 9 findings (2 blocking)`.

A review lists what the reviewer found in the diff; it does not show that the code is correct. Your review and your checks still decide.
