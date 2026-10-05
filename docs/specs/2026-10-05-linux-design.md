# Provefab on Linux servers

- Date: 2026-10-05
- Status: approved design (owner, 2026-10-05); implementation pending
- Feature: Provefab (core and Pro) runs on a headless Linux server or VM, x86_64 and ARM64, as a systemd user service, with secrets in a protected file or environment variables. macOS behaviour is unchanged.

## 1. Owner decisions

1. Target: a headless Linux server or VM (no desktop session, no browser on the machine).
2. Secrets on Linux: a file readable by its owner only, plus the existing environment variables, which take precedence.
3. Builds: GitHub Actions; Linux binaries built and attached at each `v*` tag; the notarized macOS zip stays built by hand on the owner's Mac.

## 2. What already works (inventory 2026-10-05)

Nothing in the core or Pro is gated on macOS with `cfg`. The pipeline, store, `git`/`gh` use, tracker clients, Jev client, guard and worker spawning, `flock` run lock, process groups, SIGTERM handling, the `ps`-based process-tree kill and the pre-push hook are Unix code. Pro's licence is a file (`<home>/license.key`). The macOS ties are: the Keychain through the `security` command (Jev key `provefab-typesafe`, Anthropic key `provefab-anthropic`, Jira `provefab-jira` with the e-mail in the item comment, Linear `provefab-linear`; `apiKeyHelper` for Claude Code runs `security`), the launchd service (`service.rs`), `scripts/release-macos.sh`, the landing's `install.sh` (macOS only) and the docs.

## 3. Secrets

- An interface `Secrets` (get, set, delete a named secret) with two backends, chosen by operating system at build time:
  - macOS: the Keychain through `security`, exactly as today (same item names, same behaviour).
  - Linux: `<home>/credentials.toml` (`<home>` is `PROVEFAB_HOME` or `~/.provefab`). Created with mode 0600; `run`, `doctor` and every read refuse a file whose mode grants anything to group or others, with a one-line error naming the fix (`chmod 600 <path>`). Writes are atomic (temporary file in the same directory, then rename), keep 0600 and keep other entries.
- Names: `typesafe` (Jev key), `anthropic`, `openai` (API keys for models with `auth = "api_key"`), `jira.<site>` with fields `email` and `token`, `linear`. The Jira e-mail is a field, not a Keychain comment, on Linux.
- Environment variables keep precedence on both systems: `TYPESAFE_API_KEY`, `PROVEFAB_JIRA_EMAIL` and `PROVEFAB_JIRA_TOKEN`, `PROVEFAB_LINEAR_KEY`; add `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` where the worker accepts them (verify each worker's variable in the plan).
- `provefab login <worker|jira|linear> [--api-key]` keeps its interface: masked input on the terminal, then the backend's write. No secret is ever printed, logged or passed on a command line.
- Claude Code's API key: the `apiKeyHelper` becomes `<provefab binary> secrets get anthropic`, a hidden internal subcommand on both systems, which prints that one secret to stdout and nothing else; any other name, or a missing secret, exits non-zero with no output on stdout.
- `doctor` and setup hints name the right fix per system (Keychain command on macOS, `provefab login ...` or the variable on Linux).

## 4. Service

- An interface `ServiceManager` (install, uninstall, status) with two backends:
  - macOS: launchd, as today.
  - Linux: a systemd user unit `~/.config/systemd/user/provefab.service` (`ExecStart=<home>/bin/provefab run --workers N`, `Restart=always`, `Environment=PATH=<PATH captured at install>` and `PROVEFAB_HOME`, logs to `<home>/logs/` as on macOS), managed with `systemctl --user daemon-reload`, `enable --now`, `disable --now`, `status`.
- A server needs lingering to start the user service without a login session: `service install` checks `loginctl show-user <user> -p Linger`; when it is off it prints the command (`sudo loginctl enable-linger <user>`) and does not run it. `doctor` reports lingering when a unit is installed.
- `service install` copies the binary to `<home>/bin/provefab` on both systems, as today.

## 5. Worker sign-in without a browser

- Claude: the existing `provefab login claude` flow shows a URL and accepts a pasted code, so it works with no local browser.
- Codex: the plan verifies on the installed version whether `codex login` has a mode without a local browser (device code or similar) and wires it into `provefab login codex`; if none exists, the docs say to sign in with `--api-key` on a server.
- `--api-key` works on both systems for both workers.

## 6. Distribution

- Core CI (`.github/workflows/`, public repository): on every push and pull request, `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings` and the tests on `ubuntu-latest`. On a `v*` tag: build `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` (static), run the tests for x86_64, attach `provefab-<v>-linux-x86_64.tar.gz`, `provefab-<v>-linux-aarch64.tar.gz` and their `.sha256` to the GitHub release (each archive holds `provefab-<v>-linux-<arch>/provefab`). The plan verifies that no dependency needs OpenSSL (rustls only).
- The notarized macOS zip stays built by `scripts/release-macos.sh` on the owner's Mac and is added to the same release; the release procedure is updated.
- Pro (private repository): the same build on a `v*` tag or by hand (`workflow_dispatch`), producing `provefab-pro-<v>-linux-<arch>.tar.gz` and `.sha256` as workflow artifacts; the owner uploads them to Polar with the macOS zip. Pro depends on the core tag, so the core release comes first.
- `install.sh` (landing): detect `uname -s` and `uname -m`; macOS as today; Linux x86_64 or aarch64 download the matching `.tar.gz` and `.sha256`, verify with `sha256sum` (or `shasum -a 256`), install to `~/.local/bin` without `sudo`; other systems or architectures: a clear message, nothing installed. The truncated-download guard (`main()`) stays. Tests cover macOS, Linux x86_64, Linux aarch64, an unsupported system, a wrong checksum, with a fake `uname`.

## 7. Documentation and landing

- README: requirements "macOS or Linux (x86_64, ARM64)"; the quick start covers both (install line, sign-ins, `init`, `repos add`, `doctor`, service).
- `docs/guide/configuration.md`: secrets per system; `operations.md`: the systemd user service and lingering; `security.md`: the credentials file and its mode; `trackers.md`: Jira and Linear credentials per system.
- Landing: every "macOS only" statement (FAQ, install copy, `llms.txt`, `index.md` generator, pricing lines if any) updated; one line "Linux: x86_64 and ARM64, a systemd user service".
- No em-dashes; no claim that the credentials file is encrypted (it is protected by file permissions).

## 8. Tests

- `Secrets` file backend: create with 0600, refuse a wider mode, atomic write keeps other entries, Jira fields, missing secret, environment precedence; no secret in any error or `Debug` output (R3).
- `secrets get`: prints only the named secret; unknown name or missing secret: non-zero, nothing on stdout.
- systemd backend: the generated unit text, the `systemctl` calls (fake `systemctl` and `loginctl` on `PATH`), the lingering message.
- macOS-only tests (fake `security`, fake `launchctl`) behind `cfg(target_os = "macos")`; the suite passes on macOS locally and on Linux in CI.
- `install.sh`: the cases in section 6.
- Real run before release: in a Linux VM or an Ubuntu container: the install line, `provefab login claude` without a browser, `init`, `repos add`, `doctor`, `service install` (where systemd is available), one sandbox issue end to end.

## 9. Budget and scope

- Core: at most two new modules (`secrets.rs`, and the systemd backend inside or beside `service.rs`), no migration, no new runtime dependency unless the plan shows one is required (a STOP otherwise). One new CI workflow file in each repository.
- Version 0.7.0.

Out of scope, each with a re-open trigger:

- A Docker image: a user asks to run Provefab in a container.
- Secret Service (libsecret) on Linux desktops: a desktop Linux user asks.
- Windows: a user asks.
- Signing Linux binaries: a user or distribution asks.

## 10. Decisions

1-3: owner decisions in section 1.
4. Static musl binaries (controller). Why: one binary per architecture runs on every distribution. Cost: musl's allocator is slower; re-open if a benchmark shows it matters.
5. The `apiKeyHelper` calls Provefab itself on both systems (controller). Why: one code path, no per-system shell command. Cost: a hidden subcommand that prints a secret to its own owner.
6. `service install` never runs `sudo` (controller). Why: a CLI must not escalate privileges on its own. Cost: one manual command on a new server.
