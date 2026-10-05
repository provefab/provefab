# Provefab on Linux servers

- Date: 2026-10-05
- Status: approved design (owner, 2026-10-05); implemented in 0.7.0, with the amendments recorded in section 10
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
- Task 5: no test needed a gate. The whole suite passed unchanged in `rust:1.96` (Debian, arm64): 733 passed, 13 skipped, the Linux-only tests included.

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
7. (plan) `Secrets` is an enum (`Keychain`, `File`) chosen by `Secrets::system` at build time, and both backends compile on every system. Why: async methods without boxing, `Tools` stays `Debug + Clone`, and a fake `security` drives the Keychain tests on Linux too.
8. (plan) The interface is `get`, `has`, `store`, `preflight` and `describe`, with no `delete`; departs from §3 ("get, set, delete"). Why: no command removes a secret, so `delete` would be code no user reaches. Re-open: a `provefab logout` request.
9. (plan) No `openai` secret and no `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` precedence; departs from §3. Why: Codex keeps its API-key login in its own `CODEX_HOME` on both systems, and both workers remove these variables from every stage on purpose, so a precedence would never apply. Re-open: a worker that reads its key from Provefab.
10. (plan) A new login target, `provefab login jev`, on both systems. Why: on Linux there is no Keychain command to give, and the service's environment never sees a `TYPESAFE_API_KEY` set in a shell.
11. (plan) Environment variables are applied at the call sites (`jira_auth`, `linear_key`, `typesafe_key`), not in `Secrets`; every Keychain read has the 10-second limit. Why: the call sites own their variables and fix texts; the Jev key read had no limit.
12. (plan) Only a Keychain timeout adds the `provefab login` fix to an error; a refused or malformed file names its own fix and stops `run` at start and fails `doctor`'s `credentials` line. Why: one fix per error, the right one.
13. (plan) The secrets file is TOML (`typesafe`, `anthropic`, `linear`, `[jira."<site>"]` with `email` and `token`), rewritten whole; a parse error is fixed text. Why: `toml` errors quote the offending line, which may hold a secret (R3).
14. (plan) The masked read uses `libc` termios on a terminal and reads a plain line otherwise. Why: scripts can pipe a key in; no new dependency.
15. (plan) `apiKeyHelper` is `PROVEFAB_HOME=<home> <current exe> secrets get anthropic`, each path shell-quoted; `doctor` accepts the old `provefab-anthropic` Keychain helper only where the Keychain is the store. Why: the helper reads the same file whatever the stage's environment, and a macOS install signed in before 0.7.0 keeps working without signing in again.
16. (plan) `secrets get` accepts one name, `anthropic`. Why: the only secret a worker asks Provefab for; any other name is a usage error (exit 2), a missing secret exits 1 with nothing on stdout.
17. (plan) `provefab login codex` passes `--device-auth` on Linux (verified on `codex-cli 0.156.1`); macOS keeps `codex login`. Why: the CLI's own headless sign-in.
18. (plan) `ServiceManager` is an enum (`Launchd`, `Systemd`); `Service` is renamed `Launchd`; the binary copy and the log rotation are one shared `service::prepare`, and the binary path one shared `service::binary_path`. Why: one code path per step, no caller outside `app.rs`.
19. (plan) systemd calls: `daemon-reload`, `enable` and `restart` on install, `disable --now` then `daemon-reload` on uninstall, `show --property=LoadState,ActiveState,MainPID` for status; departs from §4 (`enable --now`, `status`). Why: `enable --now` does not restart a running unit, so a reinstall would keep the old binary; `show` prints `key=value` lines and `status` exits 3 for an inactive unit.
20. (plan) The unit runs with `KillMode=mixed`, `TimeoutStopSec=30`, `Restart=always`, `RestartSec=30`, and appends both streams to `<home>/logs/run.log`. Why: SIGTERM reaches Provefab alone, which stops its workers itself, as under launchd; `append:` needs systemd 240 or later.
21. (plan) Unit values are escaped (`%`, `\`, `"`, and `$` in `ExecStart`), and a path or `PATH` holding a line break is refused before anything is written. Why: systemd reads the intended values, and nothing can inject a directive.
22. (plan) Lingering is read with `loginctl show-user <user> --property=Linger`; a failed call reads "unknown" and prints the same command; `<user>` is `$USER` when it is a plain name, else the uid. Why: `loginctl` fails for a user with no session and no lingering; `service install` never runs `sudo` (decision 6).
23. (plan) The unit directory is `$XDG_CONFIG_HOME/systemd/user` when that is absolute, else `~/.config/systemd/user`; tests inject the directory and the `systemctl` and `loginctl` paths; departs from §8 (fakes "on `PATH`"). Why: injection is how the launchd tests already work and keeps tests parallel-safe.
24. (plan) Per-system `cfg` in tests only where the system decides (`Secrets::system`, `ServiceManager::for_user`, per-system hints); departs from §8 (gate the fake `security` and `launchctl` tests). Why: with decision 7 those tests run on Linux as well.
25. (plan) CI builds each architecture natively with `musl-tools` (x86_64 on `ubuntu-latest`, aarch64 on `ubuntu-24.04-arm`), no cross-compiler. Why: no dependency needs OpenSSL (rustls with `aws-lc-rs`, bundled SQLite), both build with `musl-gcc`; proven in Docker for both architectures before CI.
26. (plan, amended by the final review) The release job never creates the GitHub release: it waits for the one the owner creates with the notarized macOS zip (every 30 s, up to 30 minutes), then uploads the Linux archives with `--clobber`. Why: one person decides when a release exists and what it says; a release made by CI first would publish without the macOS zip.
27. (plan) Archives hold `provefab-<v>-linux-<arch>/` with `provefab`, `LICENSE.md`, `provefab.example.toml` and `INSTALL.txt`; `.sha256` is `<hex>  <file>`; the tag job refuses a tag that differs from `v<crate version>`. Why: like the macOS zip, and the installer reads the first field.
28. (ruling) CI runs on a push to any branch and on pull requests, and by hand (`workflow_dispatch`), which builds, tests and packs both archives without a release. Why: prove the x86_64 musl build before the tag; a pull request branch runs the checks twice, accepted.
29. (ruling) Docker builds on the owner's Mac use `CARGO_TARGET_DIR` under `target/linux` (honoured by `scripts/package-linux.sh`) and `CARGO_BUILD_JOBS=4`. Why: the Mac's own `target/release` stays untouched, and 14 parallel links exhaust the Docker VM's 8 GB.
30. (ruling) On macOS the Jira e-mail still passes on `security`'s command line (`-j`), as before 0.7.0. Why: the e-mail is not a secret, and macOS behaviour is unchanged.
31. (ruling) User-facing wording for the file: "readable by your account only (mode 600), not encrypted". Why: root can read it too, and §7 forbids any claim of encryption.
