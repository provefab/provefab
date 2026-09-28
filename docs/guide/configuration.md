# Configuration

Provefab reads `provefab.toml` from its home directory, `~/.provefab` by default. The `PROVEFAB_HOME` variable moves that directory. A commented example sits at the repository root: [`provefab.example.toml`](../../provefab.example.toml).

`provefab doctor` checks the file and everything it depends on. Run it after every change.

## Secrets

No secret goes in `provefab.toml`.

| Secret | Where |
|---|---|
| TypeSafe (Jev) key | `TYPESAFE_API_KEY` variable, otherwise the macOS Keychain: `security add-generic-password -s provefab-typesafe -a provefab -w <key>` |
| Claude login | `provefab login claude` (directory `~/.provefab/claude`) |
| Codex (ChatGPT) login | `provefab login codex` (directory `~/.provefab/codex`) |
| GitHub | `gh auth login` (Provefab uses `gh` and `git` with your permissions) |

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

How the tier is chosen:
- Jev estimates the issue's difficulty and scope. Implementation takes the matching tier; planning and review take one tier above.
- Within a tier, Provefab takes the first free model, in file order.
- An empty tier falls back to the nearest configured one.
- Review avoids the implementer's provider when the catalog allows it.

**Tip:** list at least two providers, for example Claude and Codex, so that reviews are cross-checked by a different model family.

## `[[repos]]`: watched repositories

| Field | Default | Role |
|---|---|---|
| `slug` | required | `owner/name` on GitHub. |
| `local_path` | none | Your local clone. **Optional**: without it, Provefab clones the repository into `~/.provefab/repos/<owner>/<name>`. Either way it runs `git fetch` before every pass and starts from `origin/<base>`. |
| `label` | `provefab` | The label that triggers Provefab. Only people who can triage the repository can set it: it is the authorization. The derived labels (`<label>:in-pr`, `:needs-info`, `:failed`, `:merged`) are created at startup. |
| `base` | `main` | Branch work starts from, and pull requests target. |
| `poll_interval` | `3m` | How often the repository is polled. |
| `trust_pi_project` | `false` | Pi only: load the repository's Pi config (avoid for a repository you do not control). |
| `gates` | required, at least one | Commands run in the worktree after every implementation. All must pass. They decide, not the agents. |

Provefab Pro reads an optional `[repos.merge]` table for guarded auto-merge: `auto` (off by default), `max_lines` (400), `require_test_change` (on: only a change that adds or changes tests merges by itself), `exclude` (paths kept for a human, `.github/**` by default) and `allow_public` (off: a public repository is never merged automatically, since anyone can write its issues). This binary ignores it, says so in `provefab doctor` and in each PR comment, and never merges. The old keys `auto_merge` and `auto_merge_max_lines` are refused with a message naming `[repos.merge]`.

For gates that work well:
- Put in what your CI requires: format, lint, tests.
- Fast commands speed up every loop.
- If the repository has slow tests, keep a representative subset and let your GitHub CI do the rest.
- **Never share a build directory between tasks** (for example one `CARGO_TARGET_DIR` for every worktree). Cargo can then reuse binaries built from another task's code, and the checks pass or fail on the wrong code. Each worktree builds in its own `target/`, which the agent already warmed during implementation. To go faster safely, use a content-addressed cache such as `sccache` (`RUSTC_WRAPPER=sccache`).

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
