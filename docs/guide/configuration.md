# Configuration

Provefab reads `provefab.toml` from its home directory, `~/.provefab` by default. The `PROVEFAB_HOME` variable moves that directory. A commented example sits at the repository root: [`provefab.example.toml`](../../provefab.example.toml).

`provefab doctor` checks the file and everything it depends on. Run it after every change.

## Secrets

No secret goes in `provefab.toml`.

| Secret | Where |
|---|---|
| TypeSafe (Jev) key | `TYPESAFE_API_KEY` variable, otherwise the macOS Keychain: `security add-generic-password -s provefab-typesafe -a provefab -w <key>` |
| Claude subscription login | `provefab login claude` (directory `~/.provefab/claude`) |
| Anthropic API key | `provefab login claude --api-key`: stored in the Keychain (`provefab-anthropic`), read by Claude Code through `apiKeyHelper` (directory `~/.provefab/claude-api`) |
| Codex (ChatGPT) subscription login | `provefab login codex` (directory `~/.provefab/codex`) |
| OpenAI API key | `provefab login codex --api-key` (directory `~/.provefab/codex-api`) |
| GitHub | `gh auth login` (Provefab uses `gh` and `git` with your permissions) |
| Jira Cloud | `provefab login jira --site <site>`: e-mail and API token in the Keychain (`provefab-jira`); or `PROVEFAB_JIRA_EMAIL` and `PROVEFAB_JIRA_TOKEN` |
| Linear | `provefab login linear`: personal API key in the Keychain (`provefab-linear`); or `PROVEFAB_LINEAR_KEY` |

**Plans for work.** Provefab runs the official CLIs with whatever login you give them. For professional use, prefer a business plan (Claude Team or Enterprise, ChatGPT Business) or API keys: consumer plans can restrict commercial use (for example, Anthropic's consumer terms for EEA and Swiss residents say "Non-commercial use only"). Check your plan's terms; this is not legal advice.

## `[jev]`

| Field | Default | Role |
|---|---|---|
| `model` | required | The **full** Jev version, for example `jev-1.13.0`. `jev-latest` is refused, so that a new release never changes routing silently. `jev-1.13` (without the patch number) is rejected by the API; `provefab doctor` reports it. |
| `underspecified_threshold` | `0.7` | Above this probability that an issue is too vague, Provefab asks a question instead of coding. |
| `loop_threshold` | `0.8` | Above this, an agent judged stuck or repetitive is stopped (checked every 15 tool calls). |

Provefab still works without a Jev key: every stage runs on the `standard` tier. There is then no automatic question, no loop detector and no failure triage.

## `[[models]]`: the catalog

| Field | Default | Role |
|---|---|---|
| `id` | required | Unique name, used in logs and PR text. |
| `worker` | required | `claude-code` (the unmodified `claude` binary), `codex` (the unmodified `codex` binary) or `pi`. |
| `model` | required | Model name for that worker: a Claude Code alias (`sonnet`, `opus`, `haiku`), a Codex model (`gpt-5.5`), or a Pi `--model` pattern. |
| `provider` | `""` | Pi only: the `--provider`. |
| `tier` | required | `fast`, `standard` or `frontier`. |
| `max_concurrency` | `1` | How many stages this model may run at once. |
| `auth` | `subscription` | Claude Code and Codex only: `subscription` (your plan's login) or `api_key` (your own API key, set with `provefab login <worker> --api-key`). Pi reads its provider's own credentials and always counts as an API key. |
| `price_id` | none | The price entry to use, as `provider/model` (for example `anthropic/claude-sonnet-5-5`), when the automatic match is wrong. |
| `price_in`, `price_out` | none | USD per million input and output tokens. Set both to override the fetched prices. |
| `price_cache_read`, `price_cache_write` | the input price | USD per million cached tokens read and written, with `price_in` and `price_out`. |
| `quota_weight` | relative price | Subscription models only: how much of your plan one token uses, compared with the cheapest model of the same vendor in the catalog (1.0). It orders that vendor's models only. |

**Subscription or API key, per model.** Each model signs in its own way, so you can switch one line and run `provefab service install`. You can also list the same model twice in the same tier, once per sign-in mode: when the subscription hits its usage limit, that entry pauses and the API-key entry takes the next stages. Both still count as the same model family for cross-review. The daily budget (`max_stage_runs_per_day`) caps API spend too.

### How Provefab picks a model

1. **The tier.** Jev rates each issue once, at intake:
   - Implementation takes the tier that matches the difficulty, one tier up when Jev is unsure.
   - Planning takes the implementation tier when Jev rates the planning simple (`plan_depth` below 1.5), and one tier above otherwise. An architectural scope always plans on `frontier`.
   - Review takes the implementation tier when a subtle mistake would cost little (`review_risk` below 1.5), `frontier` when it would cost a lot (3.0 or more), and one tier above otherwise.
   - Without a Jev key, every stage runs on `standard`.
2. **The model inside the tier**, following `[routing] prefer`:
   - subscription models first, then API-key models, the cheapest first;
   - among one vendor's subscriptions, the lowest `quota_weight` first; between vendors (Claude and Codex, say), file order decides, since a weight only compares models of the same vendor;
   - a model with no known price comes after the priced ones (subscription or API key);
   - ties keep file order.
3. **The rules that come first:**
   - a stage never runs below its tier (an empty tier falls back to the nearest configured one, stronger first);
   - review avoids the implementer's model family whenever the catalog allows it, even when that family is cheaper;
   - a paused model (usage limit) is skipped;
   - an implementation that fails again after a retry moves one tier up.

`provefab log <id>` shows the tiers and, for each stage, the model and why it was chosen.

## `[routing]`

| Field | Default | Role |
|---|---|---|
| `prefer` | `subscription` | `subscription`: your plans first (already paid), API keys when they are paused. `api_key`: API keys first. `cheapest`: one list in which subscriptions count as free. |
| `prices_url` | models.dev | Where prices are fetched from, for a mirror. |
| `litellm_url` | LiteLLM's price list | The fallback source, for a mirror. |

**Prices.** Provefab fetches model prices from [models.dev](https://models.dev) at most once a day, with LiteLLM's price list as fallback, and keeps them in `~/.provefab/prices.json`. Offline, it uses that cache, or the prices built into the binary, and tries again an hour later. A price set in `provefab.toml` always wins. Claude Code aliases (`sonnet`, `opus`, `haiku`) match the newest model of that family. `provefab doctor` shows the price each model uses. The request sends no data about you or your code.

**Tip:** list at least two providers, for example Claude and Codex, so that reviews are cross-checked by a different model family.

## `[[repos]]`: watched repositories

| Field | Default | Role |
|---|---|---|
| `slug` | required | `owner/name` on GitHub, where the code and the pull requests are. |
| `local_path` | none | Your local clone. **Optional**: without it, Provefab clones the repository into `~/.provefab/repos/<owner>/<name>`. Either way it runs `git fetch` before every pass and starts from `origin/<base>`. |
| `label` | `provefab` | The label that triggers Provefab. Only people who can triage the repository can set it: it is the authorization. The derived labels (`<label>:in-pr`, `:needs-info`, `:failed`, `:merged`) are created at startup on GitHub and Linear. The risk labels (`<label>:risk-<category>`, including `:risk-unknown`) are created at startup too. Jira labels are free text and need no creation. |
| `base` | `main` | Branch work starts from, and pull requests target. |
| `poll_interval` | `3m` | How often the repository is polled. |
| `trust_pi_project` | `false` | Pi only: load the repository's Pi config (avoid for a repository you do not control). |
| `gates` | required, at least one | Commands run in the worktree after every implementation. All must pass. They decide, not the agents. |
| `post_merge_checks` | `[]` | Optional commands run against the exact merged base commit after a Provefab-created PR merges. On failure, Provefab validates a revert against the current base and opens a human-reviewed revert PR only if the reverted tree passes. It never merges the revert automatically. Enabling it later does not check old merges. Command strings may appear in GitHub comments: never put secrets on the command line, use the environment. |

Provefab Pro reads an optional `[repos.merge]` table for guarded auto-merge: `auto` (off by default), `max_lines` (400), `require_test_change` (on: only a change that adds or changes tests merges by itself), `exclude` (paths kept for a human, `.github/**` by default) `allow_public` (off: a public repository is never merged automatically, since anyone can write its issues) and `allow_risk` (risk categories that may still auto-merge; `unknown` never can). This binary ignores it, says so in `provefab doctor` and in each PR comment, and never merges. The old keys `auto_merge` and `auto_merge_max_lines` are refused with a message naming `[repos.merge]`.

For gates that work well:
- Put in what your CI requires: format, lint, tests.
- Fast commands speed up every loop.
- If the repository has slow tests, keep a representative subset and let your GitHub CI do the rest.
- **Never share a build directory between tasks** (for example one `CARGO_TARGET_DIR` for every worktree). Cargo can then reuse binaries built from another task's code, and the checks pass or fail on the wrong code. Each worktree builds in its own `target/`, which the agent already warmed during implementation. To go faster safely, use a content-addressed cache such as `sccache` (`RUSTC_WRAPPER=sccache`).

## `[repos.risk]`: risk-aware policy

Provefab classifies each round's changed files by path into risk categories. A risky change gets stricter handling: extra checks, a frontier reviewer from another provider when you have one, a visible Risk section in the PR and a label on the issue. The policy raises scrutiny; it does not prove a change is harmless. It works with no configuration, and `[repos.risk]` is optional. Path matching only: file contents are not read.

**Built-in categories** (a path matching any pattern puts the change in the category):

| Category | Patterns |
|---|---|
| `ci` | `.github/**`, `.gitlab-ci.yml`, `.circleci/**` |
| `dependencies` | `**/Cargo.toml`, `**/Cargo.lock`, `**/package.json`, `**/*lock*.json`, `**/pnpm-lock.yaml`, `**/yarn.lock`, `**/go.mod`, `**/go.sum`, `**/requirements*.txt`, `**/pyproject.toml`, `**/Gemfile*` |
| `migrations` | `**/migrations/**`, `**/*.sql` |
| `infrastructure` | `**/Dockerfile*`, `**/*.tf`, `k8s/**`, `helm/**`, `**/docker-compose*.yml` |
| `secrets-config` | `**/.env*`, `**/*.pem`, `**/*.key`, `**/*secret*` |
| `rules` | `.provefab/rules.md` (see [Repository rules](rules.md)) |

Provefab never commits `.provefab/` in a task's pull request, so the `rules` category marks a change to the rules made by other means; disable it like any built-in.

**Patterns.** `/`-separated, anchored at the repository root, case-sensitive. `**` matches any number of segments (including none), `*` any run of characters inside one segment, everything else literally. `.gitlab-ci.yml` matches only the root file; write `**/<name>` for any directory. For a rename, both the old and the new path count.

| Field | Default | Role |
|---|---|---|
| `disable` | `[]` | Built-in categories to turn off, by name. |
| `[repos.risk.categories.<name>]` | none | Adds a category, or extends a built-in of the same name. |
| `paths` | required for a new category | Patterns of the category. For a built-in, they are added to its patterns. |
| `checks` | `[]` | Commands run in the worktree, after your `gates`, when the category is detected. For a built-in, they replace its checks. A command already in `gates` runs once. Same timeout and rerun rules as gates; a failing check is an ordinary gate failure: the round goes back to implementation and counts toward the same attempt limit as other gates. |
| `reviewer_tier` | `frontier` | `frontier` or `standard`: the tier of the review when the category is detected. `frontier` applies only when a frontier model from another provider than the implementer's is configured; otherwise the usual reviewer from another provider stays (see [Usage](usage.md#risk-aware-changes)). |

```toml
[[repos]]
slug = "acme/api"
gates = ["cargo test"]

[repos.risk]
disable = ["secrets-config"]

[repos.risk.categories.auth]
paths = ["src/auth/**", "src/billing/**"]
checks = ["cargo test --test auth"]

[repos.risk.categories.migrations]
paths = ["db/schema/**"]
checks = ["./scripts/check-migration.sh"]
```

Category names use lowercase letters, digits and `-`. `unknown` and `none` are reserved: Provefab uses `unknown` when the changed files cannot be computed, and Provefab Pro's calibration report uses `none` for changes with no risk category.

**Validation errors** (an invalid `[repos.risk]` is refused when the configuration loads, so Provefab does not start; `provefab doctor` and `provefab run` report `<slug>: [repos.risk]: <message>`):

- `unknown category in disable: <name>`: `disable` names something that is not built-in.
- `invalid category name: <name>`: empty, `unknown`, `none`, or not lowercase letters, digits and `-`.
- `<name>: empty path`, `<name>: empty check`: a blank entry.
- `<name>: path "<p>" must be relative, without "./", a trailing "/" or empty segments`: a pattern that can never match a changed path (`/src/**`, `./src/**`, `src/`, `src//auth`).
- `<name>: no paths`: a new category without `paths`.
- `<name>: reviewer_tier must be standard or frontier`.
- `<name> is disabled`: the same built-in is in `disable` and in `categories`.
- An unknown key under `[repos.risk]` or a category table is refused too.

`provefab doctor` prints one `risk <slug>` line per repository: the category count and names, and how many checks were added. It ends with `; warning: risky changes implemented on <providers> keep a standard reviewer (no frontier model on another provider)` when, for one or more providers in your catalog, no frontier model is on another provider. Providers are listed in catalog order and comma-separated (for example `claude-code`, `codex`, `pi:<provider>`). See [Usage](usage.md#risk-aware-changes) for what happens to a risky change.

## `[repos.tracker]`: Jira or Linear issues

Absent, the repository's issues are its GitHub issues. Setup, labels and limits: [Jira and Linear](trackers.md).

| Field | Default | Role |
|---|---|---|
| `kind` | `github` | `github`, `jira` or `linear`. |
| `site` | none | Jira only, required: the site's host name, such as `acme.atlassian.net` (no `https://`, no path). |
| `project` | none | Jira and Linear, required: the project or team key, `[A-Z][A-Z0-9_]*`, as in `ENG-123`. |

Refused at load: an unknown `kind` or key (credentials never go in this file), `site` outside Jira, `project` with `github`, and, for Jira, a `label` containing whitespace.

Switch a repository's `kind` (or its `project`) only when the repository has no task history that could collide: `provefab run` and `provefab add` refuse to start while a task in progress (including one whose pull request is still watched), or an update still to send, belongs to the previous tracker. A new ticket whose number matches an earlier task of the repository is skipped with a log line. See [Jira and Linear](trackers.md#limits).

## `[limits]`

| Field | Default | Role |
|---|---|---|
| `stage_timeout` | `30m` | Longest an agent stage (plan, implement, review) may run. |
| `gate_timeout` | `20m` | Longest one check command may run. |
| `max_turns` | `plan 40, implement 150, review 40` | Turn limit per stage. |
| `review_rounds` | `2` | Correction rounds the review may ask for before a new pass. |
| `max_auto_passes` | `3` | Automatic new passes per issue before Provefab asks you. |
| `max_stage_runs_per_day` | `60` | Worker runs over a rolling 24 hours, across all repositories. |
| `max_drive_steps` | `200` | Most steps one task takes in one go (a safety net). |
| `retry_delays` | `["5m", "15m", "45m"]` | Waits after a transient failure (network, GitHub). After the last one, Provefab asks you. |

## Changing the configuration

- **Service running:** edit the file, then run `provefab service install --workers N`. It reloads the service and takes the current `PATH`.
- **New tool installed** (`claude`, `codex`, `cargo`...): same command, so the service finds it.
- **New repository:** add a `[[repos]]` block, run `provefab doctor`, then `provefab run --dry-run` to see how its issues would be handled.
