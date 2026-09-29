# Handoff — Provefab feature 1: post-merge verification

Date: 2026-09-29  
Status: implementation drafted locally, tests passing, v0.1.1 binaries built and notarized locally; **nothing published or deployed**.

## Product direction agreed

Provefab should identify needs existing coding-agent products do not solve, rather than compete with no_human on the existing issue-to-PR workflow. Target: small engineering teams already using coding agents. Four product opportunities were selected, to implement one by one:

1. Post-merge verification and safe rollback.
2. Durable evidence and decision record.
3. Reviewer-quality calibration.
5. Risk-aware engineering policy engine.

Feature 1 was selected first. Scope agreed: monitor only Provefab-created PRs; run explicitly configured repository commands against the merged base commit; if these fail, open a revert PR only when the reverted tree passes the same checks; never merge the revert automatically. Verification is opt-in. PRs with more than one commit, conflicts, unsafe attribution, or failed revert checks require human intervention.

## What is already present before feature 1

The core already implements the pre-merge pipeline: repository-defined `gates`, a bug reproduction command, test-tampering detection, independent model review, and evidence in the opened PR. `provefab status`, `log`, and `stats` report task/review/merge information. Feature 1 adds an optional post-merge stage; it does not replace pre-merge verification.

## Work completed locally

Main repo: `provefab/`

- Added approved design/spec: `docs/specs/2026-09-29-post-merge-verification-design.md`.
- Added optional `[[repos]].post_merge_checks`, empty by default, with validation, example config, docs, and doctor visibility.
- Added migration `crates/provefab/migrations/0004_post_merge.sql` and durable per-task/merge check records.
- Extended GitHub PR metadata capture to include PR head, squash merge SHA, base branch, and commit count.
- On merge, queues a check only for a known Provefab task and configured matching base. It uses the exact merge SHA and avoids retroactively checking old merges after opt-in.
- Scheduler runs post-merge checks in detached worktrees and writes local result evidence. Check results and revert URLs appear in `status`, `log`, and `stats`.
- Failures attempt a revert from the current base; conflicts, multi-commit PRs, modified verification trees, and revert-check failures fail closed and notify on the issue and PR. Successful revert checks lead to a human-reviewed revert PR only.
- Added tests in `crates/provefab/tests/post_merge.rs` for success, failure/revert, conflict, rollback check failure, retry after PR creation failure, interrupted verification, old merges, merge attribution, and multi-commit safety.
- Updated core README, configuration/usage/operations docs, Pro README, and landing FAQ/hero/competitor copy.
- Bumped core and Pro package versions to 0.1.1. Pro manifest is restored to use the public git tag `v0.1.1`; its lockfile is restored to pre-tag state, because the tag does not exist yet.

Private Pro repo: `provefab-pro/`

- Pro itself did not need feature code changes; its build was exercised against the local core 0.1.1 by temporarily substituting a path dependency. Its manifest was restored afterward.
- Pro README describes the new core behavior.

Landing repo: `landing/`

- Updated FAQ and hero to mention opt-in post-merge checks and human-reviewed revert PRs.
- Updated the no_human comparison prose and handoff note.
- Landing docs are generated from `landing/vendor/provefab`; the checked-out submodule remains on the old core revision, so the published `/docs` output currently does **not yet include** the new core docs.

## Verification completed

Core `provefab/`:

- `cargo fmt -- --check` passed.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo nextest run --status-level fail`: 333 passed, 11 skipped.

Pro `provefab-pro/`:

- Tested and linted against local path dependency: 25 unit tests + 16 integration tests passed; `cargo fmt -- --check` and clippy passed.

Landing `landing/`:

- `pnpm test`: 23 license Worker tests + 28 site tests, Astro build, and internal link check passed. Astro emits an existing warning that the `i18n` content collection is absent.

Artifacts built from local sources:

- `provefab/dist/provefab-0.1.1-macos-universal.zip` — signed, notarization accepted, SHA verified. Latest SHA-256: `d044ba81fd6ea6b1f7a41de940af1be619fd847a3ac6228e819ba09297c99229`.
- `provefab-pro/dist/provefab-pro-0.1.1-macos-universal.zip` — signed, notarization accepted, SHA verified. Latest SHA-256: `1f6c5f593e7fedff748101e3edcbb3dfabbec807ef312007d9f1a743a9f6b72f`.

The full repo test run previously had a flaky lock test fail once; subsequent nextest run passed it. Latest validated full run is all passing.

## Important unfinished items / review concerns

Do not describe this as production-ready yet. Review and address these before release:

1. **Safely compare one-commit PR to squash merge SHA.** Current logic checks GitHub PR `commits` count equals one and then reverts the reported merge SHA. Validate GitHub `mergeCommit` semantics on a real squash-merged PR and ensure SHA ancestry/base checks are adequate.
2. **Multi-step state transitions and crashes.** Audit every crash window: initial check row, running state, verification result, revert branch/push, PR creation, and notification. The implementation retries an open-revert path after PR creation errors by detecting/reusing its branch/PR through the generic `pr_create` behavior, but verify no duplicate branch/PR or unsafe branch reuse.
3. **Check state semantics.** `failed` is used as a resumable “verification failed; proceed to prepare revert” state; stale `running` is cleaned and re-run; `blocked` and `revert_open` are terminal. Consider explicit states such as `verification_failed` and `preparing_revert` to make the state machine clearer and safer.
4. **Notifications.** Verify marker-based dedupe for both issue and PR comments under partial failures, including issue comment succeeding while PR comment fails. Ensure retries do not create duplicate issue comments; tests should cover this.
5. **Untrusted command output.** Ensure summaries never leak secrets. Commands run with gate credential scrubbing, but output tails and summaries should be reviewed for accidental secrets before posting any detail to GitHub.
6. **Bounded database summaries.** Confirm failure summaries/output sizes are bounded; large output is kept in local sessions, GitHub receives only safe concise summaries.
7. **Current-base races.** Base may advance during revert construction/testing/push. Add a final fetch/compare to ensure the proposed revert remains based on current default branch immediately before PR creation; if moved, restart safely or block.
8. **Revert PR idempotency and branch name collisions.** A task/SHA-derived branch identifies the intended rollback, but ensure any existing branch is verified to be exactly the expected revert and not unrelated work. Ensure generic PR reuse only returns a same-repo PR to the configured base.
9. **Retained artifacts.** `~/.provefab/post-merge/` may retain worktrees when external commands fail early. Ensure cleanup/recovery handles both success and all error paths.
10. **Docs/landing consistency.** Landing published docs are generated from its submodule; update that pointer only after the core source is ready/published, then rebuild the site.
11. **Pro lock/build.** After core `v0.1.1` is published, update Pro `Cargo.lock` from the tag, run tests/lints/build using its pinned tag dependency, and rebuild the Pro ZIP from the final locked manifest. Existing Pro ZIP was built using the temporary local core path and must be considered provisional until rebuilt against the tag.
12. **Public copy claims.** Review comparison claims and post-merge wording for accuracy. This functionality validates configured repository commands; it is not deploy/production health monitoring.

## Remaining release steps for feature 1

Do not publish/deploy without explicit owner approval. When authorized:

1. Finish the safety/recovery audit and add tests for each found edge case.
2. Run final core checks and produce final signed/notarized core ZIP.
3. Commit/publish core source and tag `v0.1.1` (tag needed before the Pro pinned dependency resolves).
4. Point Pro at core tag `v0.1.1`, update `Cargo.lock`, run its tests/lints/build, regenerate its signed/notarized ZIP and checksum.
5. Update landing submodule to the released core revision/tag so generated `/docs` contains post-merge docs; re-run `pnpm test`.
6. Only with approval, publish GitHub releases/artifacts and deploy the landing site. Do not infer deployment approval from artifact preparation.
7. Run an end-to-end real GitHub smoke test in a disposable/private repository: one passing check, then one failing check; ensure only a safe revert PR is opened and never merged by Provefab.

## Next product feature: #2 evidence and decision record

Once feature 1 is complete/released or otherwise accepted, proceed with #2 before #3. #2 is the likely foundation for calibrating reviewer quality.

Suggested first step: inspect existing `stage_outputs`, `stage_runs`, routing decisions, review findings, `provefab log`, and PR body data. Write a spec (do not code until approved) for a durable per-change record that captures:

- issue/PR/commit identities and timestamps;
- agent/model/provider by stage and cost;
- plan/acceptance criteria;
- check commands, exit status, and evidence references;
- reviewer verdict and findings;
- human disposition of each finding (accepted, rejected, fixed, waived);
- merge/revert/post-merge result;
- privacy/retention limits, output redaction, exportability, and schema evolution.

The record should distinguish claims from agents/reviewers from deterministic facts. Avoid calling test passage proof of correctness. Keep a durable append-only history or versioned events so feature #3 can measure reviewer precision/recall against later human and post-merge outcomes.

## Feature #3: reviewer calibration

Build on evidence/decision records:

- collect human disposition of review findings;
- compare findings with later defects/regressions;
- report precision/acceptance rates with sample sizes and uncertainty, stratified by provider/model and task/risk category;
- support replay/golden-set evaluation before routing/policy changes;
- do not claim recall unless there is a credible labelled set of missed issues.

## Feature #5: risk-aware policy engine

Add policy tiers by change risk, e.g. auth, billing, migrations, secrets, CI, dependencies, public API, infrastructure. Map risks to required checks, reviewers, deployment validation, human approval and auto-merge prohibition. Begin as explainable, fail-closed, repo-configurable policy; use feature #2/#3 evidence before policies make adaptive decisions.

## Working tree / publication caution

There are three separate git repositories: `provefab/`, `provefab-pro/`, and `landing/`. Working copies contain user changes; inspect `git status` before edits. Do not reset or overwrite existing changes. No commits, tags, GitHub releases, or site deploys have been made by this work.
