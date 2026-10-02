# Setup an AI agent can run

- Date: 2026-10-02
- Status: approved design (owner, 2026-10-02); implementation pending
- Feature: sub-project 1 of "AI agent friendly". A coding agent (Claude Code, Codex) can write and check Provefab's configuration for a repository by itself, with commands that need no editor and output it can read; the human keeps installation, sign-ins, keys and the service. The site offers a one-line installer and a sentence to paste into an agent.

## 1. Intent

Today the quick start asks a person to copy the example configuration and edit it by hand, then read `provefab doctor`. An agent can do the configuration part reliably only with non-interactive commands, deterministic detection and machine-readable checks. The rest (installing the binary, signing workers in, the Jev key, the service) stays with the person, who gets the exact commands to run.

## 2. Owner decisions

1. Scope of the agent: configuration only. The agent never installs the binary or the service, never handles a secret, never labels an issue, never starts `provefab run`.
2. Gate commands come from detection only; no override flags at creation (a repository that is not recognised is configured by hand).
3. The site offers two copyable starts in tabs: a terminal installer and a sentence for an agent.

## 3. `provefab init`

- Writes `~/.provefab/provefab.toml` (the home from `PROVEFAB_HOME` when set) when it does not exist; refuses with exit code 3 and changes nothing when it exists.
- Model catalog from the worker CLIs found on `PATH`: `claude` gives `claude-sonnet` (standard) and `claude-opus` (frontier); `codex` gives `codex-gpt` (standard). Same entries as `provefab.example.toml`. No CLI found: exit code 4 with the install hint; nothing written.
- No repository is added; the file says to run `provefab repos add`.
- `--dry-run` prints the file and writes nothing.
- The written file loads with the same loader as `provefab run` (checked before writing).

## 4. `provefab repos add <owner/name>`

- Requires the configuration file; adds one `[[repos]]` block at the end of the file without rewriting the rest (comments and layout kept). A repository already configured (any case) is refused with exit code 3.
- Source of the repository's files: `--path <checkout>` when given (a local clone of that repository), otherwise read through `gh` without cloning (default branch, and the few files detection needs).
- Base branch: the repository's default branch.
- Stack detection at the repository root, exactly one stack:
  - Rust (`Cargo.toml`): `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
  - Node (`package.json`): the package manager from the lock file (`pnpm-lock.yaml` pnpm, `yarn.lock` yarn, otherwise npm); gates are `<pm> run lint`, `<pm> run typecheck`, `<pm> test` for the scripts that exist, in that order; no script among them: not recognised.
  - Python (`pyproject.toml`, or `setup.py`/`setup.cfg` with a `tests` directory): `ruff check .` when ruff is configured (`[tool.ruff]` or `ruff.toml`), then `pytest`.
  - Go (`go.mod`): `go vet ./...`, `go test ./...`.
- No stack or several stacks: exit code 4, an error naming what was found and pointing to the configuration guide; nothing written.
- The block: `slug`, `label = "provefab"`, `base`, `gates`; nothing else (trackers, risk, post-merge checks are configured by hand).
- The whole file, with the block, is validated by the loader before writing; `--dry-run` prints the block and writes nothing.

## 5. `provefab doctor --json`

- One JSON object per line per check: `name`, `ok`, `detail`, and `fix` when a command fixes it (for example `provefab login claude`, `gh auth login`, `security add-generic-password -s provefab-typesafe -a provefab -w`). Without `--json` the output is unchanged.
- Exit code 0 when every check passes, 1 otherwise (with or without `--json`).
- No secret appears in any field.

## 6. Exit codes and messages

0 success; 1 a doctor check failed; 2 usage error; 3 configuration or repository already exists; 4 stack or worker not recognised; 5 `gh` or network error. Errors are one line on stderr starting with `provefab:`. Codes are documented in `--help` of the new commands and in the agent guide.

## 7. The agent guide

`docs/guide/agents.md`, plain Markdown written for agents, linked first in the README and from the site's `llms.txt`:

- If `provefab` is not installed: ask the person to run the installer line (section 8) and stop.
- Run `provefab init` (exit 3 means a configuration exists: keep it), `provefab repos add <owner/name> --path <checkout>` from the repository the person is in, then `provefab doctor --json`.
- For each failed check, show the person its `fix` command to run themselves, then run `doctor --json` again.
- Never: handle a secret, install the binary or the service, label an issue, run `provefab run`.
- A repository not recognised (exit 4): report the error and point the person to the configuration guide.
- End with a short summary: what is configured, and the commands left for the person.

## 8. Site (landing repository)

- `site/public/install.sh`, served at `https://provefab.com/install.sh`: macOS only (other systems: a clear message, exit 1); resolves the latest release through the GitHub API; downloads `provefab-<version>-macos-universal.zip` and its `.sha256`; verifies with `shasum -a 256`; mismatch stops with nothing installed; installs `provefab` to `~/.local/bin` by default (`PROVEFAB_INSTALL_DIR` to change), no `sudo`, no shell profile edits; says when the directory is not on `PATH`; prints the next steps (sign-ins, `provefab init`, `provefab repos add`, `provefab doctor`) without running them; `set -eu`, temporary directory removed. Tested with a fake release served locally (normal, wrong checksum, non-macOS, directory not on `PATH`).
- Landing hero: a two-tab copy block with a copy button: "Terminal" `curl -fsSL https://provefab.com/install.sh | sh`; "With an agent" `Set up Provefab on this repository: follow https://provefab.com/docs/agents/`; one line under it saying the agent writes the configuration while sign-ins, keys and the service stay with you.
- The docs sync publishes `agents.md` at `/docs/agents/`.

## 9. Documentation

- Core: `docs/guide/agents.md`; README quick start uses `provefab init` and `provefab repos add` (step 4) and links the agent guide; `configuration.md` mentions both commands and detection; `operations.md` exit codes.
- No em-dashes; no claim that setup is automatic beyond what is described.

## 10. Tests

- Detection per stack from fixture directories (Rust, pnpm, yarn, npm with and without scripts, Python with and without ruff, Go, none, several), both from `--path` and through `gh` (FakeHub serving files).
- `init`: catalog from fake CLIs on `PATH`, no CLI, existing file refused, `--dry-run`, written file loads.
- `repos add`: appended block, comments kept, duplicate refused (any case), whole file validated, `--dry-run`.
- `doctor --json`: line shape, `fix` present for sign-in and key checks, exit codes 0 and 1.
- Exit codes end to end through the binary.
- Real run: an agent configures the sandbox repository in an isolated home following only `agents.md`.

## 11. Budget and scope

- Core: exactly one new module (`setup.rs`), no migration, no new dependency. More is a STOP.
- Version 0.6.0.

Out of v1, each with a re-open trigger:

- Overriding detected gates at creation: owners ask for it.
- Linux installer: Provefab supports Linux.
- Homebrew tap: users ask for it.
- Agents operating Provefab day to day (sub-project 3), site readability for agents (sub-project 2).

## 12. Decisions

1-3: owner decisions in section 2.
4. Append the repository block as text instead of rewriting the TOML (controller). Why: keeps the person's comments and layout without a new dependency. Cost: the block always goes at the end of the file.
5. Install to `~/.local/bin` without `sudo` (controller). Why: a pasted command should not ask for an administrator password. Cost: some users add the directory to `PATH`.
