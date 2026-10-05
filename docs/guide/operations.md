# Operations

## The service

Provefab runs as a background service: a launchd agent on macOS, a systemd user service on Linux. It starts by itself and restarts if it stops.

```bash
provefab service install --workers 1   # install or update, then (re)start
provefab service status                # running (pid N), loaded, not installed
provefab service uninstall             # stop and remove the service
```

`install` does three things:
- it copies the current binary to `~/.provefab/bin/provefab`, so rebuilding the repository never breaks the service;
- it writes the service: `~/Library/LaunchAgents/dev.provefab.run.plist` on macOS, `~/.config/systemd/user/provefab.service` on Linux (then `systemctl --user daemon-reload`, `enable` and `restart`);
- it freezes the `PATH` of the terminal you run it from.

**On a Linux server**, a user service runs only while you are logged in, unless lingering is on for your account. `install` checks it and, when it is off, prints the command to run once with administrator rights: `sudo loginctl enable-linger <user>`. Provefab never runs `sudo` itself. `provefab doctor` shows a `lingering` line while the service is installed. Logs go to `~/.provefab/logs/run.log` on both systems (`journalctl --user -u provefab` shows systemd's own messages). The service file needs systemd 240 or later.

**Run `install` again** after:
- installing a new Provefab version (the zip or the Linux archive from the GitHub Releases page, or `cargo install --git https://github.com/provefab/provefab --tag <version> --locked provefab`);
- installing or moving a tool (`claude`, `codex`, `gh`, `cargo`...);
- editing `provefab.toml`.

Only one process works on the queue at a time, thanks to the `~/.provefab/run.lock` lock, shared by `provefab run` and the commands that need the queue. To run a pass by hand while the service runs, stop the service first.

`--workers N` sets how many tasks run in parallel. Each model stays limited by its `max_concurrency`, often 1 for a subscription.

## Without the service

```bash
provefab run                 # continuously in the terminal (Ctrl-C stops cleanly)
provefab run --once          # one pass: everything that can move, then exit
provefab run --dry-run       # classification and routing only, nothing changes
```

Ctrl-C and SIGTERM (what launchd and systemd send when they stop the service) cancel the running stages and kill the agents' processes, so `provefab service uninstall` and a stop of the service end the run cleanly. Tasks resume at the next start, in the state they were in.

## Where things are

| Path | Contents |
|---|---|
| `~/.provefab/provefab.toml` | the configuration |
| `~/.provefab/provefab.db` | the state (SQLite): tasks, transitions, routing, stages, outputs, and the record of each change (`change_events`, `findings`) |
| `~/.provefab/logs/run.log` | the service log; `provefab service install` rotates it to `run.log.1` once it passes 10 MB |
| `~/.provefab/repos/` | the clones Provefab manages |
| `~/.provefab/worktrees/<id>/` | a task's worktree, removed after the merge |
| `~/.provefab/sessions/<id>/` | agent transcripts and check outputs, per stage |
| `~/.provefab/post-merge/` | temporary detached worktrees, one per check step, removed when the step ends |
| `~/.provefab/claude/`, `~/.provefab/codex/` | worker plan logins, kept apart from your own sessions |
| `~/.provefab/claude-api/`, `~/.provefab/codex-api/` | worker API-key sign-ins (the Anthropic key itself stays in the Keychain or `credentials.toml`) |
| `~/.provefab/credentials.toml` | on Linux, the Jev key, the Anthropic key and the Jira and Linear credentials (mode 600) |
| `~/.provefab/prices.json` | model prices, refreshed at most once a day from models.dev (LiteLLM as fallback) |
| `~/.provefab/bin/provefab` | the binary the service runs |

Provefab never deletes remote `provefab/revert-*` branches.

The record (`change_events`, `findings`) is kept without limit. `provefab prune --before YYYY-MM-DD --yes` removes the record rows of finished tasks and the old `maintenance_runs` rows (not the latest of each repository and kind, nor runs that opened a pull request); the tasks themselves stay. `provefab export` never contains command output, which stays in `~/.provefab/sessions/`.

`PROVEFAB_HOME` moves all of this elsewhere.

## Budgets and safeguards

Provefab stops itself rather than spend quota in a loop:

- **Automatic passes:** at most `max_auto_passes` per issue (3 by default). Beyond that, the task goes to `needs_you`.
- **Daily work:** at most `max_stage_runs_per_day` worker runs over a rolling 24 hours (60 by default). Beyond that, tasks wait in `waiting`, with one comment per issue, and resume by themselves.
- **Rate limits:** a rate-limited provider is paused (15 minutes, doubled on each repeat, 4 hours at most). Stages move to another model of the same tier, or wait.
- **Transient failures** (network, GitHub, busy repository): waits of 5, 15 then 45 minutes, then `needs_you`.
- **Steps per task:** at most `max_drive_steps` in one go (200 by default).

## Exit codes

`provefab init`, `provefab repos add` and `provefab doctor` exit with:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a `doctor` check failed (the Jev key is optional and never fails it), or another error |
| 2 | usage error, or `repos add` before `provefab init` |
| 3 | the configuration or the repository already exists |
| 4 | stack or worker not recognised |
| 5 | `gh` or network error |

A wrong argument or option prints the command's usage and exits 2. Every other error is one line on stderr starting with `provefab:`.

## Troubleshooting

Always start with `provefab doctor`. Each `FAIL` line says what to do. `provefab doctor --json` prints the same checks and warnings as one JSON object per line (`name`, `ok`, `detail`), with `fix`, the command to run, for sign-ins and keys.

| Symptom | Likely cause | Fix |
|---|---|---|
| `jev ... Unknown model` | `jev.model` without its patch number | use the full version, for example `jev-1.13.0` |
| `jev key missing` | no key | `provefab login jev` (on macOS also `security add-generic-password -s provefab-typesafe -a provefab -w`) |
| `credentials FAIL ... can be read by group or others` | `credentials.toml` was copied or edited with a wider mode | `chmod 600 ~/.provefab/credentials.toml` |
| `lingering FAIL` (Linux) | the service stops when you log out and does not start at boot | `sudo loginctl enable-linger <user>`, once |
| `service install` fails with `Failed to connect to bus` (Linux) | no user systemd session in this shell (for example after `su`) | log in over SSH as that user, or set `XDG_RUNTIME_DIR=/run/user/$(id -u)` |
| `claude login FAIL` | Claude session missing or expired | `provefab login claude` |
| `codex guard hook FAIL` | the Codex guard hook is not trusted | `provefab login codex` |
| `claude api key FAIL` | a model has `auth = "api_key"` but no key is stored, or the API-key config dir does not read it | `provefab login claude --api-key` |
| `codex api login FAIL` or `codex api guard hook FAIL` | Codex is not signed in with an API key in `~/.provefab/codex-api` | `provefab login codex --api-key` |
| a model's plan keeps hitting its usage limit | the subscription is paused until the limit resets | add the same model with `auth = "api_key"` in the same tier: it takes over while the plan is paused |
| `model ... removed: its worker is not ready` in the log | its worker is not installed or not signed in; Provefab dropped it from the catalog at startup | fix the matching `doctor` line, then `provefab service install` |
| every task in `waiting` | every model of the tier is paused, or the daily budget is reached | `provefab status` gives the reason; wait, or widen the catalog or the budget |
| the service cannot find `claude` or `cargo` | `PATH` frozen at install time | run `provefab service install` again from a terminal where the tool is found |
| `another Provefab process holds the queue lock` | the service or another command that needs the queue is running; wait for it to exit | `provefab service uninstall` before a manual pass |
| `price <id>: no price` in `doctor` | the model matched no known price (a new model, a typo, or a Pi provider models.dev does not list); it still runs, ranked after priced models, and its cost is not counted | set `price_id = "provider/model"`, or `price_in` and `price_out` |
| `prices: snapshot` or an old cache in `doctor` | the machine could not reach models.dev or LiteLLM | nothing to do: routing uses the cache or the built-in prices; the service tries again an hour later |
| `rules <slug> FAIL` in `doctor` | `.provefab/rules.md` is invalid (the line says why) or could not be read; tasks run without rules meanwhile | fix the file in a pull request and merge it (see [Repository rules](rules.md)); `doctor` reads the base branch as last fetched |
| `[repos.merge] is read by Provefab Pro` warning | the config asks for auto-merge | expected with this binary: PRs open and wait for you |

To dig into a task:
- read `provefab status` for how many post-merge checks are in each state (if opted in);
- read `provefab log <id>` for a check's detail: state, failure kind, merged SHA and any revert PR URL;
- read the transcripts in `~/.provefab/sessions/<id>/`;
- read the check outputs (`gates-N` directories).
