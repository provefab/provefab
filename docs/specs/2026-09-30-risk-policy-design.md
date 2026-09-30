# Risk-aware policy

- Date: 2026-09-30
- Status: implemented on feature/risk-policy; release pending
- Feature: #5 of the product direction. Core part (this repo) plus a Pro part (`provefab-pro`), described together because the core carries the policy and Pro only reads it at merge time.

## 1. Intent

Changes that touch sensitive areas (CI, dependencies, migrations, infrastructure, secret-looking files, and the repository's own sensitive paths such as auth or billing) get stricter handling, and every decision is explained in the pull request. v1 is deterministic, repo-configurable and fails closed. It classifies by changed paths only; adaptive policies built on the evidence record (#2) and calibration (#3) come later.

## 2. Owner decisions

1. A risk category changes what happens (classify then harden): extra checks, a frontier reviewer, a visible Risk section and label, and, in Pro, no auto-merge.
2. Detection, display, extra checks and the frontier reviewer are in the free core (safety should not be paywalled); merge rules are in Pro, next to auto-merge.
3. Categories come from changed paths, with built-in defaults a repository can extend or disable.
4. Architecture A: a declarative policy and a pure classifier in the core; consequences applied at existing pipeline points; Pro reads categories at merge time.

## 3. Built-in categories

| Category | Paths |
|---|---|
| `ci` | `.github/**`, `.gitlab-ci.yml`, `.circleci/**` |
| `dependencies` | `**/Cargo.toml`, `**/Cargo.lock`, `**/package.json`, `**/*lock*.json`, `**/pnpm-lock.yaml`, `**/yarn.lock`, `**/go.mod`, `**/go.sum`, `**/requirements*.txt`, `**/pyproject.toml`, `**/Gemfile*` |
| `migrations` | `**/migrations/**`, `**/*.sql` |
| `infrastructure` | `**/Dockerfile*`, `**/*.tf`, `k8s/**`, `helm/**`, `**/docker-compose*.yml` |
| `secrets-config` | `**/.env*`, `**/*.pem`, `**/*.key`, `**/*secret*` |

A detected category's default consequence: reviewer tier `frontier`, no extra check (commands are repository-specific).

## 4. Configuration

```toml
[repos.risk]
disable = ["dependencies"]            # built-in categories turned off

[repos.risk.categories.auth]          # a repository category
paths = ["src/auth/**", "src/session/**"]
checks = ["cargo test auth"]          # added to the gates when detected
reviewer_tier = "frontier"            # optional; default "frontier"

[repos.risk.categories.migrations]    # extend a built-in
paths = ["db/schema/**"]              # added to the built-in paths
checks = ["./scripts/check-migration.sh"]
```

- `[repos.risk]` is optional; absent, the built-ins apply.
- A table under `categories` with a built-in name extends it (its paths are added; its `checks` and `reviewer_tier` apply); a new name creates a category.
- Validation at load (a config error, like the others): an unknown name in `disable`; a category with no paths (for a new category); an empty path or check; a `reviewer_tier` other than `standard` or `frontier`; a category name outside `[a-z0-9-]+`; the reserved names `unknown` and `none` (Pro's calibration uses `none` for "no risk"); a path with a leading `/` or `./`, a trailing `/` or an empty segment (it can never match). (amended 2026-09-30 after final review)
- `provefab doctor` prints one `risk <slug>` line per repository: the number of categories and their names, and the total number of checks. (amended 2026-09-30 after final review) When no frontier model has a provider different from a standard model's (or there is no frontier model), the line ends with `; warning: no frontier model from a second provider, risky changes keep a standard cross-provider reviewer`.

## 5. Path patterns

A small matcher in the core, no new dependency: patterns are `/`-separated; `**` matches any number of segments (including zero); `*` matches any run of characters inside one segment; other characters match literally. Every pattern is anchored at the repository root: `.gitlab-ci.yml` matches only the root file, and a name meant for any directory is written `**/<name>` (as the built-ins do). Matching is case-sensitive, on repository-relative paths with `/` separators.

## 6. Classification

- `risk::classify(policy, paths) -> Vec<Detected>` is pure: for each category (in a stable order: built-ins in table order, then repository categories by name), the list of paths that matched. Categories with no match are absent.
- Input paths: every path `Git::changed_files(base...HEAD)` reports, including both sides of a rename.
- Run after each implementation round, on the actual diff. (amended 2026-09-30 in the plan) Classification runs in the gate step right after the round is committed, because the diff exists only then; the categories' extra checks run as a second gates pass named `risk-gates`, after the repository's own gates. If the changed paths cannot be computed, the result is the single category `unknown` (fail closed: treated as a category needing a frontier reviewer, see §7.2; Pro never auto-merges it) and the round continues.
- Each classification is recorded as a `risk_classified` event (source `fact`, schema_version 1): `{ pass, round, categories: [{ name, paths }] }`. The record's export treats `paths` as identity (not free text).

## 7. Consequences in the core

1. Gates: the union of the detected categories' `checks` is appended to the repository's gates for that round, deduplicated, in category order; same timeout and rerun rules; a failure is an ordinary gate failure (back to implementation). The stage run and `gates_run` event record them like other gates.
2. Review: when any detected category (or `unknown`) has `reviewer_tier = "frontier"`, the review stage runs on the frontier tier (a new rule in `review_tier`, after the existing ones), provided the catalog has a frontier model whose provider differs from the provider of the model that implemented the round (the latest `implement` stage run). Otherwise the tier the other rules give is kept, so the usual cross-provider reviewer applies; nothing parks for lack of a frontier model. (amended 2026-09-30 after final review: owner decision, option 1) The earlier rule sent the task to `needs_you` when no frontier model was configured; it is removed.
3. PR body: a "Risk" section, one line per category: `- migrations: \`migrations/0005.sql\`, \`db/schema/users.sql\` · checks added: \`./scripts/check-migration.sh\` · reviewer: frontier` (paths listed up to 5, then "and N more"). No section when nothing is detected. When a category wants frontier but no frontier model from another provider than the implementer's exists, its line ends with `reviewer: standard (no frontier reviewer from another provider is configured)`; the section uses the same decision as the review tier. (amended 2026-09-30 after final review: owner decision, option 1)
4. Labels: `<label>:risk-<category>` for each detected category, created like the other Provefab labels; stale risk labels from an earlier round are removed. (amended 2026-09-30 in the plan) The labels go on the issue like every Provefab label, created on demand with `ensure_label`, and are applied right after classification, before the extra checks run.
5. `provefab log` shows the risk classification of each round.

## 8. Provefab Pro

Pro (paid, separate repository) reads the categories through the public `risk::resolve` and `risk::classify` functions on the exact head it would merge, so no risky change is merged automatically unless the repository explicitly allows that category; `unknown` is never allowed. Pro's calibration report can split its figures by risk category. Pro's rules are specified in the Pro repository.

## 9. Documentation and landing

Committed with the feature; deployed at the release.

- Core: `docs/guide/configuration.md` (`[repos.risk]`, built-ins, validation), `docs/guide/usage.md` (Risk section, labels, extra checks, frontier reviewer), `README.md` (one sentence), `provefab.example.toml` (a commented example).
- Pro: its own README (in the Pro repository).
- Landing: Free list in `Pricing.astro` gains "Risk-aware checks: migrations, CI, dependencies and your own sensitive paths get stricter review"; the Pro merge-policies line states that risky changes never merge without a person unless allowed; one FAQ entry. Terms: no change (no new Pro feature stops at expiry; merge policies are already covered by "auto-merge").
- Checks: landing `pnpm test`; no em-dashes; no "secure" or "safe" guarantees (the policy raises scrutiny; it does not prove safety).

## 10. Tests

- Matcher: `*`, `**`, anchoring, examples (`db/migrations/0005.sql`, `.env.local`, `web/package-lock.json`, `src/auth/mod.rs`) and non-matches (`src/authz.rs` against `src/auth/**`).
- Policy resolution: built-ins, `disable`, repository category, extending a built-in, every validation error.
- Classification: several categories, matched paths, no match, rename both sides, `unknown` when paths cannot be computed.
- Pipeline (existing fakes): a change under `migrations/` adds the category check to the gates, forces a frontier reviewer from another provider, adds the Risk section and label, records `risk_classified`; a failing risk check goes back to implementation; a frontier model only from the implementer's provider, or none, keeps the standard cross-provider reviewer and says so in the PR; `unknown` when the changed files cannot be computed; a later round without the category removes its label. (amended 2026-09-30 after final review: owner decision, option 1)
- Pro: tested in the Pro repository.
- Non-regression: a repository with no detected risk behaves exactly as before (same gates, tiers, PR body).

## 11. Budget and scope

- Core: exactly one new module `risk.rs`; no migration (the event uses the record); no new `Hub` method (labels use the existing `edit_labels` / `ensure_label`).
- Pro: budget stated in the Pro repository.
- A second new module, a migration or a new `Hub` method is a STOP: ask the owner.

Out of v1, each with a re-open trigger:

- Diff-content patterns (`password`, `DROP TABLE`...): paths prove insufficient in real use.
- Classifying the issue before implementation: a pilot wants to forbid autonomous work on an area.
- Two model families for a category: a pilot asks for it.
- Adaptive policies from calibration: `--by risk` shows enough labelled data.
- Deployment validation: Provefab gets deployment access.

## 12. Decisions

1-4: owner decisions in section 2.
5. Classification runs on the actual diff after each implementation round, not on the plan's predicted files (controller, reversible). Why: the plan can be wrong; the diff is what merges.
6. `unknown` when the diff cannot be computed, with a frontier reviewer (per §7.2) and no auto-merge (controller). Why: fail closed.
7. Default consequence of a category is a frontier reviewer only; checks are opt-in per repository (controller). Why: commands are repository-specific; a wrong default command would fail every run.
8. Pro reclassifies at merge time with the core's function instead of reading the last event (controller). Why: the merge decision must describe the exact head being merged.
9. No new dependency for glob matching (controller). Why: the pattern language is small and must be explainable in the docs.
10. A risky review is frontier only when a frontier model from another provider than the implementer's exists; otherwise the standard cross-provider reviewer stays and the PR and `provefab doctor` say so (owner decision 2026-09-30 after final review, option 1). Why: owner's choice among the options the final review listed; it keeps the reviewer on another provider than the implementer. Re-open if pilots want risky changes to wait for a frontier reviewer instead.
