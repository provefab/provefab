# Repository rules

- Date: 2026-10-01
- Status: approved design (owner, 2026-10-01); implementation pending
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

## 4. Reading

- Rules are read from the task's base commit (`git show <base>:.provefab/rules.md` in the task's checkout), never from the worktree: an agent that edits the file in its own pull request does not change the rules it works under. A pass reads them once, at its start, and records `rules_loaded { pass, numbers, sha256 }` (source `fact`).
- New module `crates/provefab/src/rules.rs`: `parse(text) -> Result<Vec<Rule>, RulesError>`, `Rule { number, summary, paths, text }`, `select(rules, stage, files) -> Vec<&Rule>`, `render(selected, budget) -> (String, truncated)`.

## 5. Use in the stages

- Plan prompt: every rule (the plan decides the files).
- Implementation and review prompts: rules without `paths:`, plus rules whose patterns match a file in the plan's file list (implementation) or the round's changed files (review).
- Rendering: a block titled `Repository rules (approved by the maintainers of this repository)` placed outside the UNTRUSTED markers, each rule as `R<n>: <summary>` and its text. Budget: 12 000 characters; rules beyond it are left out in number order and the prompt says how many were left out; the event records it.
- Rules are guidance to the agents and stay under the guard: a rule cannot allow a tool call the guard refuses.
- Review: the review output schema gains an optional `rule` field per finding (`"R3"`); the review prompt asks the reviewer to report a violation of a selected rule as a finding with that field. A `rule` value that is not a selected rule's number is dropped (the finding stays, without it).
- Storage: migration `0007` adds `findings.rule TEXT` (NULL when none). Review notes in the pull request show `F2 · R3 · ...`; `provefab log` and `provefab export` show the rule.
- Pull request body: a `Rules:` line under Checks listing the rules given to the round's review (`Rules: R1, R3`), or nothing when there are none.

## 6. Risk category

A new built-in risk category `rules` with path `.provefab/rules.md` (same defaults as the others: frontier reviewer from another provider when available, no extra check). A task pull request that changes the rules is therefore labelled, gets a Risk line, and Pro never auto-merges it unless allowed. A repository can disable it like any built-in.

## 7. Periodic extension point

- `ReviewPolicy` gains `fn periodic<'a>(&'a self, repo: &'a RepoConfig, tools: &'a dyn PeriodicTools) -> BoxFuture<'a, Result<(), String>>`, default no-op. The scheduler calls it once per repository per day (the first tick after midnight local time, and at startup if the last call is older than 24 hours), never concurrently with itself for a repository, and logs an `Err` without stopping the service.
- `PeriodicTools` (implemented by the core):
  - `signals(since: i64) -> Vec<Signal>`: the repository's record since a time: rejected and waived finding dispositions with their reason, findings that cite a rule with their disposition, change requests from pull requests closed with a comment, gate failures (command and task), reverts and reopens after a merge, and the outcome of earlier periodic pull requests (merged, closed).
  - `rules_at_base() -> Result<Option<String>, ...>`: the current rules file on the base branch (fresh fetch).
  - `ask_model(prompt, schema) -> Result<serde_json::Value, ...>`: one structured call on the standard tier, routed like a stage (subscription first, then API keys by price), run by the worker in an empty temporary directory under the guard with no repository access; its cost is recorded.
  - `propose_file(path, content, title, body) -> Result<String, ...>`: commits the file on a dedicated branch from the current base, pushes it and opens a pull request, or updates the open pull request of the same branch; never merges; returns the URL.
  - `last_run(kind) -> Option<MaintenanceRun>` and `record_run(kind, outcome, pr_url)`.
- Storage: migration `0007` adds `maintenance_runs (id, repo, kind, started_at, finished_at, model_id, cost_usd, quota_units, outcome, pr_url)`; `provefab status` shows the last run per repository and kind.
- The signals' free text goes to the model provider like any stage prompt; command output is never included; secrets are redacted as in `provefab export`.

## 8. Doctor and visibility

- `provefab doctor` prints one `rules <slug>` line: the number of rules on the base branch, or the validation error, or `none`. For a public repository the line adds `; rules are instructions to the agents: review pull requests that change .provefab/rules.md closely`.
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
