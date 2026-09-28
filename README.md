# Provefab

Provefab turns labelled GitHub issues into tested, reviewed pull requests.

It runs on your Mac as a service. AI coding agents write the code (Claude Code, Codex or Pi, each through its own unmodified CLI and your own login), and Jev, TypeSafe's classifier, picks the right model for each stage. Everything that decides an outcome is deterministic Rust: the checks, the commit, the push and the pull request.

```
issue labelled `provefab`
  -> Jev classification (kind, difficulty, scope) and model choice
  -> plan (read-only) -> implement (every tool call filtered by the guard)
  -> your repository's checks (your commands: fmt, lint, tests...)
  -> review by a different model provider
  -> Provefab commits and pushes, opens the PR, comments on the issue
```

The pull request then waits for you. Guarded auto-merge (a second reviewer on another model, then deterministic merge conditions) is part of Provefab Pro and is not in this repository.

## Quick start

Requirements: macOS, Rust 1.96 or newer, `git` and `gh` (signed in with `gh auth login`), and at least one worker: `claude` (Claude Code) or `codex` (Codex CLI).

```bash
# 1. Install the binary (it embeds the worker plugins).
cargo install --path crates/provefab

# 2. Sign the workers in, once, in Provefab's own config directories.
provefab login claude        # Claude account (subscription or key), in ~/.provefab/claude
provefab login codex         # ChatGPT account, in ~/.provefab/codex, and trust for the guard hook

# 3. Jev (TypeSafe) key, in the macOS Keychain. Without a key, Provefab runs
#    with cautious defaults.
security add-generic-password -s provefab-typesafe -a provefab -w <your-key>

# 4. Configure (see provefab.example.toml), then check.
mkdir -p ~/.provefab && cp provefab.example.toml ~/.provefab/provefab.toml
$EDITOR ~/.provefab/provefab.toml
provefab doctor

# 5. Try it without changing anything: classify and route the open issues.
provefab run --dry-run

# 6. Start the service (starts at login, restarts by itself).
provefab service install --workers 1
```

Then put the `provefab` label on an issue. Provefab picks it up at its next poll (every 3 minutes by default). Read an issue before you label it: the label is what authorizes agents to spend time on its text (see [Security](docs/guide/security.md)).

## Day to day

| To... | Run |
|---|---|
| see every task and why it is in its state | `provefab status` |
| see everything about one task (routing, stages, checks, plan, reviews) | `provefab log <id>` |
| queue an issue by hand, or restart a stopped task | `provefab add <issue-url>` |
| check tools, logins, Jev and repositories | `provefab doctor` |
| see whether the service runs | `provefab service status` |
| follow the service live | `tail -f ~/.provefab/logs/run.log` |
| stop the service | `provefab service uninstall` |

Provefab talks to you on GitHub, in issue comments that always start with *Posted by Provefab*. Labels track progress:

| Label | Meaning |
|---|---|
| `provefab` | to do (you set it: it is the authorization) |
| `provefab:needs-info` | Provefab asked a question; answer in a comment |
| `provefab:in-pr` | a pull request is open |
| `provefab:merged` | the pull request was merged |
| `provefab:failed` | Provefab stopped; its comment says why |

## Documentation

- [Configuration](docs/guide/configuration.md): every field of `provefab.toml`, with its default.
- [Usage](docs/guide/usage.md): writing issues that land, answering Provefab, restarting, reading states.
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
