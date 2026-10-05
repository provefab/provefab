# Provefab

Provefab turns labelled GitHub, Jira or Linear issues into pull requests that arrive green, reviewed by a second model, with their evidence.

[![Provefab in 12 seconds: a force push denied, a failing test and a blocking review sent back, then a pull request waiting for your review](docs/assets/provefab-loop.gif)](https://provefab.com/#demo)

It runs as a service on your Mac or on a Linux server (x86_64 or ARM64). AI coding agents write the code (Claude Code, Codex or Pi, each through its own unmodified CLI, signed in with your own plan or your own API key, chosen per model), and Jev, TypeSafe's classifier, rates each issue so that every stage runs on the cheapest model that can do it: your plans first, then your API keys by price, with prices updated daily. Everything that decides an outcome is deterministic Rust: the checks, the commit, the push and the pull request.

```
issue labelled `provefab`
  -> Jev classification (kind, difficulty, scope, planning depth, review risk)
  -> cheapest capable model per stage
  -> plan (read-only) -> implement (every tool call filtered by the guard)
  -> your repository's checks (your commands: fmt, lint, tests...)
  -> review by a different model provider
  -> Provefab commits and pushes, opens the PR, comments on the issue
```

Provefab keeps a local record of what each change observed, claimed and decided, exportable as JSON Lines.

Changes that touch CI, dependencies, migrations, infrastructure, secret-looking files or paths you name get stricter review: extra checks, a frontier reviewer from another provider when one is configured, a Risk section in the PR and a label on the issue (see [Configuration](docs/guide/configuration.md#reposrisk-risk-aware-policy)).

A repository can keep its conventions in `.provefab/rules.md`: every stage gets them and the reviewer reports a change that breaks one (see [Repository rules](docs/guide/rules.md)).

The pull request then waits for your click. Optionally set `post_merge_checks` per repository: after a Provefab PR merges, Provefab runs your commands on the merged commit; on failure it can open a human-reviewed revert PR (unless a later commit already fixed it), but only when the reverted tree passes the same checks. It never merges a revert automatically. This checks repository commands, not deployed production health. Provefab Pro adds a second reviewer on another model, merge policies (auto-merge small, tested changes under fail-closed conditions), cost reports per repository and model, reviewer calibration reports, and rule proposals drafted from your decisions; it is not in this repository.

## Quick start

Requirements: macOS or Linux (x86_64, ARM64; on Linux, the `ca-certificates` package for HTTPS and, for the service, systemd 240 or later), `git` and `gh` (signed in with `gh auth login`), and at least one worker: `claude` (Claude Code) or `codex` (Codex CLI).

With a coding agent, point it at [Set up Provefab with a coding agent](docs/guide/agents.md): it writes and checks the configuration, and leaves the sign-ins, keys and service to you.

```bash
# 1. Install the binary (it embeds the worker plugins), one of:
#    a. the installer (macOS or Linux, into ~/.local/bin, no sudo):
curl -fsSL https://provefab.com/install.sh | sh
#    b. a release archive, no Rust needed: from the Releases page,
#       provefab-<version>-macos-universal.zip (signed and notarized) or
#       provefab-<version>-linux-<x86_64|aarch64>.tar.gz (static), unpack, then
mkdir -p ~/.local/bin && install -m 755 provefab ~/.local/bin/provefab
#    c. from source, with Rust 1.96 or newer:
cargo install --git https://github.com/provefab/provefab --tag v0.7.0 --locked provefab

# 2. Sign the workers in, once, in Provefab's own config directories.
#    On a server without a browser, Claude shows a link and takes the code
#    you paste; Codex shows a device code to enter on another machine.
provefab login claude        # Claude plan login, in ~/.provefab/claude
provefab login codex         # ChatGPT plan login, in ~/.provefab/codex, and trust for the guard hook
# ...or your own API keys, for models with auth = "api_key" (see docs/guide/configuration.md):
provefab login claude --api-key
provefab login codex --api-key

# 3. Jev (TypeSafe) key: in the macOS Keychain, or on Linux in
#    ~/.provefab/credentials.toml (readable by your account only, mode 600,
#    not encrypted). Without a key, Provefab runs with cautious defaults.
provefab login jev

# 4. Configure, then check. `init` writes ~/.provefab/provefab.toml with the
#    models of the workers it finds; `repos add` detects your checks
#    (add --path <your clone> to read it instead of GitHub).
provefab init
provefab repos add your-account/your-repo
provefab doctor

# 5. Try it without changing anything: classify and route the open issues.
provefab run --dry-run

# 6. Start the service: a launchd agent on macOS, a systemd user service on
#    Linux (on a server, also run the `sudo loginctl enable-linger` command it prints).
provefab service install --workers 1
```

Then put the `provefab` label on an issue. Provefab picks it up at its next poll (every 3 minutes by default). Read an issue before you label it: the label is what authorizes agents to spend time on its text (see [Security](docs/guide/security.md)).

## Day to day

| To... | Run |
|---|---|
| see every task and why it is in its state | `provefab status` |
| see everything about one task (routing, stages, checks, plan, reviews) | `provefab log <id>` |
| queue an issue by hand, or restart a stopped task | `provefab add <issue-url>` |
| have a pull request a person wrote reviewed | label it `provefab:review`, or comment `/provefab review` |
| check tools, logins, Jev and repositories | `provefab doctor` |
| add a repository, with its checks detected | `provefab repos add <owner/name>` |
| measure: PRs opened, merged (automatically or by hand), reviewers | `provefab stats` |
| export the record as JSON Lines | `provefab export` |
| delete old records | `provefab prune --before YYYY-MM-DD --yes` |
| see whether the service runs | `provefab service status` |
| follow the service live | `tail -f ~/.provefab/logs/run.log` |
| stop the service | `provefab service uninstall` |

Provefab talks to you on the issue's tracker (GitHub, Jira or Linear), in comments that always start with *Posted by Provefab*, and opens the pull request on GitHub. Labels track progress:

| Label | Meaning |
|---|---|
| `provefab` | to do (you set it: it is the authorization) |
| `provefab:needs-info` | Provefab asked a question; answer in a comment |
| `provefab:in-pr` | a pull request is open |
| `provefab:merged` | the pull request was merged |
| `provefab:review` | on a pull request a person wrote: review it (you set it) |
| `provefab:failed` | Provefab stopped; its comment says why |
| `provefab:risk-<category>` | the change touches that risk category |

## Documentation

- [Set up with a coding agent](docs/guide/agents.md): what an agent runs to configure Provefab, and what it leaves to you.
- [Configuration](docs/guide/configuration.md): every field of `provefab.toml`, with its default.
- [Usage](docs/guide/usage.md): writing issues that land, answering Provefab, restarting, reading states.
- [Repository rules](docs/guide/rules.md): conventions in `.provefab/rules.md`, given to every stage and checked by the reviewer.
- [Pull request reviews](docs/guide/pr-review.md): reviewing pull requests people wrote, on request.
- [Jira and Linear](docs/guide/trackers.md) (beta): issues from a Jira Cloud project or a Linear team, pull requests on GitHub. When Linear paging stops early, the log line says why: `linear issues: stopped after N pages (page cap)` or `linear issues: stopped after N pages (repeated cursor)`.
- [Operations](docs/guide/operations.md): the service, logs, budgets, troubleshooting.
- [Security](docs/guide/security.md): what agents can and cannot do, and who can trigger Provefab.

## How the code is organized

| Crate | Role |
|---|---|
| `crates/provefab` | the `provefab` binary and library: pipeline, scheduler, CLI, SQLite store, git and GitHub, guard, service |
| `crates/agent-workers` | the three workers (Pi, Claude Code, Codex): launch, event stream, clean shutdown |
| `crates/jev` | TypeSafe (Jev) API client |
| `plugins/` | integrations embedded in the binary: Pi extension, Claude Code hook, shared skills |

The pipeline asks a `ReviewPolicy` how many approvals a change needs and what to do once its PR is open. This repository ships `OpenPrOnly`: one approval, then the PR waits for a person.

Checks before any change:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
```

Tests that launch real agents are ignored by default. See `crates/provefab/tests/real_workers.rs` to enable them.

## License

[Functional Source License 1.1, Apache 2.0 future license](LICENSE.md) (FSL-1.1-ALv2). You may use, modify and redistribute Provefab for any purpose except offering a competing product or service. Each version becomes available under the Apache License 2.0 two years after its release.

Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).
