# Repository rules

- Date: 2026-10-01
- Status: approved design (owner, 2026-10-01); implemented in Provefab 0.4.0, with the amendments marked in each section
- Feature: a repository keeps its own rules in `.provefab/rules.md`; Provefab reads them from the base branch, gives them to every stage, and has the reviewer report violations. Proposing new rules from the evidence record is a Provefab Pro feature, specified in the Pro repository; this spec defines the public extension point it uses.

## 1. Intent

Provefab forgets: a finding a person rejected for a good reason, a change a person requested when closing a pull request, a gate that keeps failing for the same cause, all come back on the next task. Repository rules are the team's conventions written down once, applied by every stage and checked by the reviewer, so the same correction is not made twice.

## 2. Owner decisions

1. The rules live in a versioned file in the repository, `.provefab/rules.md`; approval is the merge of the pull request that changes it.
2. New rules are drafted by a standard-tier model from the evidence record, with at least two concordant signals per rule, in one grouped pull request at most once a week per repository (Pro).
3. Rules are guidance in the plan, implementation and review prompts, with an optional path scope, and the reviewer reports violations as findings that cite the rule.
4. Edition: reading, injecting and checking rules is free (core); learning and proposing rules is Pro.
5. Architecture: a daily periodic extension point in the core scheduler, used by Pro (approach 1 of 3).

## 3. File format

```markdown
## R3: Errors in the API layer use ApiError, never anyhow
paths: src/api/**
sources: PR #41 F2 (rejected), PR #57 (closed with a change request)

Return `ApiError` from handlers; `anyhow` stays in the CLI.
```

- A rule is a level-2 heading `## R<n>: <summary>`, then optional `paths:` (comma-separated patterns, the risk policy's matcher and validation rules) and `sources:` (free text for traceability, never interpreted), then a blank line and the rule text (Markdown, until the next `## ` heading).
- Text before the first rule heading is ignored (a human introduction is allowed).
- Numbers are positive integers, unique in the file; gaps are allowed; a number is never reused (documented; Pro enforces it when it proposes).
- Limits: at most 100 rules; a summary at most 120 characters; a rule text at most 2000 characters.
- Validation errors (malformed heading, duplicate number, invalid pattern, a limit exceeded, a `paths:` or `sources:` line out of place) make the whole file invalid. An invalid file never stops a task: the task runs without rules, the task's log records `rules_invalid` with the reason, and `provefab doctor` prints it.
- A missing file means no rules (no message).
- Format details (amended 2026-10-01 in the plan): blank lines between a heading and its `paths:` or `sources:` lines are allowed, and the blank line before the text is optional; in the introduction a `## ` heading is ignored unless it starts with `## R` and a digit (a malformed rule heading there is an error); after the first rule, every `## ` line is a rule heading; `R0` and leading zeros are malformed; `paths:` values are split on commas and trimmed; Windows line endings and a byte order mark read like plain text. `RulesError` never quotes the file (line numbers, rule numbers and pattern positions only).
- (amended 2026-10-01 during implementation): fenced code blocks in a rule are tracked (an opening fence of 3 or more backticks or tildes, closed by the same character at least as long): a `## ` or `paths:` line inside one is rule text. A fence still open at the end of the file makes the file invalid (`UnclosedFence`, naming the opening line). The introduction is not tracked.

## 4. Reading

- Rules are read from the task's base commit (`git show <base>:.provefab/rules.md` in the task's checkout), never from the worktree: an agent that edits the file in its own pull request does not change the rules it works under. A pass reads them once, at its start, and records `rules_loaded { pass, numbers, sha256, omitted }` (source `fact`). (amended 2026-10-01 in the plan): the pass's file is kept in a `rules` stage output `{pass, base, text}` written with the event in one transaction, and every stage of the pass reads it (a pass begun before the upgrade, or whose base is pinned again, loads the file at its next stage); `omitted` is the plan prompt's count of rules left out by the budget; a missing file records no event; an unreadable file (a git failure) is recorded as `rules_invalid` for that pass, with fixed text.
- New module `crates/provefab/src/rules.rs`: `parse(text) -> Result<Vec<Rule>, RulesError>`, `Rule { number, summary, paths, text, span }` ((amended 2026-10-01 in the plan): `span` is the byte range of the rule's section, for Provefab Pro, which edits a rule and leaves every other byte as written), `select(rules, stage, files) -> Vec<&Rule>`, `render(selected, budget) -> (String, truncated)`.

## 5. Use in the stages

- Plan prompt: every rule (the plan decides the files).
- Implementation and review prompts: rules without `paths:`, plus rules whose patterns match a file in the plan's file list (implementation) or the round's changed files (review).
- Rendering: a block titled `Repository rules (approved by the maintainers of this repository)` placed outside the UNTRUSTED markers, each rule as `R<n>: <summary>` and its text. Budget: 12 000 characters; rules beyond it are left out in number order and the prompt says how many were left out; the event records it. (amended 2026-10-01 in the plan): rules are sorted by number, and the first rule that does not fit and every later one are left out.
- Rules are guidance to the agents and stay under the guard: a rule cannot allow a tool call the guard refuses.
- Review: the review output schema gains an optional `rule` field per finding (`"R3"`); the review prompt asks the reviewer to report a violation of a selected rule as a finding with that field. A `rule` value that is not a selected rule's number is dropped (the finding stays, without it). (amended 2026-10-01 in the plan): a `rule` must name a rule the review was given (selected and not left out by the budget), and is stored as `R<n>`.
- Storage: migration `0007` adds `findings.rule TEXT` (NULL when none). Review notes in the pull request show `F2 · R3 · ...`; `provefab log` and `provefab export` show the rule.
- Pull request body: a `Rules:` line under Checks listing the rules given to the round's review (`Rules: R1, R3`), or nothing when there are none. (amended 2026-10-01 in the plan): the line comes from a `rules_given` stage output `{pass, round, numbers}` recorded when the review prompt carried rules, read for the PR's pass and round; it sits after the check lines.

## 6. Risk category

A new built-in risk category `rules` with path `.provefab/rules.md` (same defaults as the others: frontier reviewer from another provider when available, no extra check). A task pull request that changes the rules is therefore labelled, gets a Risk line, and Pro never auto-merges it unless allowed. A repository can disable it like any built-in. (amended 2026-10-01 in the plan): the guard already refuses agent writes to `.provefab/**` and `commit_all` unstages `.provefab/` before every task commit, so a task pull request never changes the file by Provefab's own doing; the category marks a change made by other means (a branch pushed by a person, say).

## 7. Periodic extension point

- `ReviewPolicy` gains `fn periodic<'a>(&'a self, repo: &'a RepoConfig, tools: &'a dyn PeriodicTools) -> BoxFuture<'a, Result<(), String>>`, default no-op. The scheduler calls it once per repository per day (the first tick after midnight local time, and at startup if the last call is older than 24 hours), never concurrently with itself for a repository, and logs an `Err` without stopping the service. (amended 2026-10-01 in the plan): the call runs as a background task beside the queue; each call is recorded as a `periodic` maintenance run (`ok` or `error: <message>`), which is how the startup rule survives a restart; a day change while the previous call still runs is skipped; local time comes from `localtime_r`; `run --once` waits for the calls it started. (amended 2026-10-01 during implementation): a start at 23:59 after more than 24 hours gives two calls minutes apart (one for the startup rule, one for the day change).
- `PeriodicTools` (implemented by the core; (amended 2026-10-01 in the plan): object safe, and every method returns `Result<_, String>` because each reads the store or git, which can fail; (amended 2026-10-01 during implementation): errors are fixed text, and the raw git or `gh` detail goes to the local log only, redacted):
  - `signals(since: i64) -> Vec<Signal>`: the repository's record since a time: rejected and waived finding dispositions with their reason, findings that cite a rule with their disposition, change requests from pull requests closed with a comment, gate failures (command and task), reverts and reopens after a merge, and the outcome of earlier periodic pull requests (merged, closed). (amended 2026-10-01 after the final review, I4): when the state of one of those pull requests cannot be read, `signals` fails with `could not read <url>`. Why: skipping it would show a closed proposal as open, losing its refusal.
  - `rules_at_base() -> Result<Option<String>, ...>`: the current rules file on the base branch (fresh fetch).
  - `ask_model(prompt, schema) -> Result<serde_json::Value, ...>`: one structured call on the standard tier, routed like a stage (subscription first, then API keys by price), run by the worker in an empty temporary directory outside every checkout; its cost is recorded. (amended 2026-10-01 in the plan): the call goes through the same claim as a stage (`[routing] prefer` order, cooldowns, `max_concurrency`, refused when the daily stage budget is spent) but is not a `stage_runs` row, so it does not count toward `max_stage_runs_per_day`; its cost waits until `record_run`. (amended 2026-10-01 during implementation): every tool call is refused (a no-tools stage: `PROVEFAB_NO_TOOLS`, `provefab guard --no-tools`, which lets only the structured answer through), so the model gets only the prompt. Why: the prompt carries untrusted comment text, the answer can reach a public pull request body, and the guard never blocked reads by absolute path, so "read-only tools in an empty directory" would not have kept the repository out of the call.
  - `propose_file(path, content, title, body, last_pushed) -> Result<Proposal, String>`: commits the file on a dedicated branch from the current base, pushes it and opens a pull request, or updates the open pull request of the same branch; never merges. (amended 2026-10-01 in the plan): the branch is `provefab/<file stem>` (`provefab/rules`), rebuilt from the current base in a fresh detached worktree, committing only that file; the pull request's title and body are updated with the new `Forge::pr_edit`, since `pr_create` reuses an open pull request without touching it; `Proposal { sha, pr }` returns the pushed commit and the pull request (or why it could not be opened or updated). (amended 2026-10-01 during implementation): the push is leased (`--force-with-lease` against the `sha` Provefab last pushed, which the caller passes as `last_pushed`): when the branch holds anything else, a person changed it, and Provefab pushes and edits nothing while the branch holds that commit, until the branch is deleted (GitHub can delete it on merge) or the commit is merged into the base (amended 2026-10-01 after the final review). Why: never lose a maintainer's commit. (amended 2026-10-01 after the final review, I1): the commit message carries the trailers `Provefab-Proposal: <path>` and `Provefab-Content-Sha256: <hex sha256 of the content>`; a branch head other than `last_pushed` is still replaced when it is an ancestor of `origin/<base>` (merged, nothing is lost) or when it is Provefab's own unrecorded commit: one parent, that parent in `origin/<base>`, only `<path>` changed, both trailers present and the blob at `<path>` hashing to the trailer's value (a maintainer's amend keeps the trailers but not the hash). A push that errored but reached `origin` returns its sha. The error of a person's change starts with `rules::CHANGED_BY_A_PERSON`. Why: an abort, a SIGTERM or a failed `record_run` after the push would otherwise block proposals forever. (amended 2026-10-01 after the final review, M6): when `path` is the rules file, `propose_file` compares the file at the fresh base with what `rules_at_base` returned on the same tools and, if it differs, pushes nothing and fails with `<path> changed on the base branch of <slug> since it was read, so Provefab pushed nothing`. Why: the content was built from the older text and would silently revert a maintainer's merged edit.
  - `last_run(kind) -> Option<MaintenanceRun>` and `record_run(kind, outcome, pr_url, detail)`. (amended 2026-10-01 in the plan): `detail` is the policy's own JSON, never shown, stored in a `detail` column of `maintenance_runs` (migration `0007`); `highest_rule_number()` returns the highest rule number the record saw, loaded by a pass or cited by a finding, so a number is never reused.
- Storage: migration `0007` adds `maintenance_runs (id, repo, kind, started_at, finished_at, model_id, cost_usd, quota_units, outcome, pr_url, detail)`; `provefab status` shows the last run per repository and kind, and hides a `periodic` run whose outcome is `ok`.
- The signals' free text goes to the model provider like any stage prompt; command output is never included; secrets are redacted as in `provefab export`. (amended 2026-10-01 in the plan): signal ids are `pr#<n>/F<k>`, `pr#<n>/c<i>` (change requests), `task#<id>/gates`, `task#<id>/revert-<check>`, `task#<id>/reopen-<pass>` and `pr#<n>/merged|closed`; human and reviewer text is sent in clear, because hashed text, as in the export, would make the signals useless; credential-looking strings are replaced by `<redacted>`; a gate failure lists configured commands only (`gates` and risk checks), never the plan's model-written reproduction command. The outcome of every earlier periodic pull request is returned whatever its age, so a refusal keeps suppressing a proposal.
- `app::open_pipeline` (amended 2026-10-01 in the plan): builds the pipeline `run` uses, without the run lock, so Provefab Pro's command can run next to the service. (amended 2026-10-01 after the final review, I5): `commands::lock_run(paths)` is the one owner of `<home>/run.lock`; `provefab run` and Pro's `rules propose` both take it, so the command refuses while the service runs. Why: two processes proposing at once would race on the proposal branch and record two runs.
- No-tools calls on Codex (amended 2026-10-01 after the final review): besides the guard's refusal and the read-only sandbox, the worker passes `-c web_search="disabled" -c features.view_image=false`, checked against Codex's source (`create_web_search_tool` returns no tool for `disabled`; `view_image` is registered only while the feature is on) and accepted by codex-cli 0.156.1.
- Redaction (amended 2026-10-01 after the final review, M8): credential query parameters in URLs (`token`, `key`, `secret`, `password`, `access_token`, `api_key`) and every Slack token kind (`xox` prefix) are redacted; over-redaction of a lowercase hex word or the word after "bearer" is accepted. The fetch error `refresh_checkout` logs is redacted too.

## 8. Doctor and visibility

- `provefab doctor` prints one `rules <slug>` line: the number of rules on the base branch, or the validation error, or `none` (amended 2026-10-01 in the plan): doctor reads the base as last fetched and never fetches (a rules file merged a minute ago shows after the next fetch); a managed repository not cloned yet says `none yet: the repository is cloned on its first task`; an invalid or unreadable file is a `FAIL` line. For a public repository, or one whose visibility cannot be read (fail closed), the line adds `; rules are instructions to the agents: review pull requests that change .provefab/rules.md closely`.
- `provefab log` shows `rules_loaded` (numbers, and how many were left out by the budget) and `rules_invalid`.

## 9. Documentation and landing

- Core: new `docs/guide/rules.md` (format, scope, how stages use rules, the reviewer check, the risk category, security notes, how to write a rule by hand); `usage.md`, `configuration.md` (`rules` category), `security.md`, `README.md`.
- Pro: its README (in the Pro repository).
- Landing: one Free line (repository rules read and checked on every task), one Pro line (Provefab proposes your team's rules from its decisions), one FAQ entry; terms: rule proposals join the Pro features that stop at expiry (FR and EN), flagged for the owner's legal review.
- No em-dashes; no claim that rules guarantee correct code.

## 10. Tests

- Parse: valid file, introduction text, every validation error, limits, missing file.
- Reading: rules come from the base commit; an agent's edit of the file in the worktree changes nothing for that pass.
- Selection and rendering: path scope per stage, budget truncation with the count, outside the UNTRUSTED markers.
- Review `rule` field: stored, shown in review notes, log and export; an unknown rule number dropped.
- Invalid file: the task runs without rules and records `rules_invalid`.
- Risk category `rules`; doctor line, public-repository note.
- Periodic: called once per day per repository, at startup after 24 hours, never concurrently; an error is logged and the service continues; `PeriodicTools` methods against fakes (signals content, `propose_file` creates then updates, never merges; cost recorded; `last_run`/`record_run`).
- Non-regression: a repository without the file behaves exactly as before (prompts, PR body, review notes).
- Real run: on `provefab/provefab`, a hand-written rules file applied by a task; then Pro's proposal (Pro spec).

## 11. Budget and scope

- Core: exactly one new module (`rules.rs`), one migration (`0007`), no new dependency. A second module, a second migration or a new dependency is a STOP.
- Version 0.4.0.

Out of v1, each with a re-open trigger:

- Rules as executable gates: a pilot asks for it.
- Rules shared across repositories: a pilot with several repositories asks for it.
- Learning from reviews written in GitHub's interface (not `/provefab` commands): `/provefab` commands prove insufficient in real use.

## 12. Decisions

1-5: owner decisions in section 2.
6. Rules are read from the base commit, not the worktree (controller). Why: an agent must not be able to change its own instructions inside its pull request. Cost if wrong: a rule added on the task's own branch by a person does not apply until merged.
7. An invalid file runs the task without rules instead of stopping it (controller). Why: a typo in a convention file must not halt the queue; the log and doctor make it visible. Cost if wrong: a team relying on a rule gets one task without it until the file is fixed.
8. Rules are trusted instructions placed outside the UNTRUSTED markers, still under the guard (controller). Why: they are merged by maintainers; the guard keeps the hard limits. Cost if wrong: on a public repository a merged malicious rule steers the agents within the guard's limits; doctor says so.
9. Rule violations reuse findings with a `rule` column rather than a separate table (controller). Why: dispositions, calibration and the record already work on findings. Cost: none.
