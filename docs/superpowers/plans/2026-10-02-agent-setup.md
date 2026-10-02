# Agent Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A coding agent can write and check Provefab's configuration for a repository by itself: `provefab init` writes `provefab.toml` with the models of the worker CLIs on `PATH`, `provefab repos add <owner/name>` appends one `[[repos]]` block with the base branch and gates detected from the repository's files, `provefab doctor --json` prints one JSON line per check with the command that fixes it, every new command has documented exit codes, and `docs/guide/agents.md` tells an agent what to run and what to leave to the person.

**Architecture:** One new module, `crates/provefab/src/setup.rs`, holds everything the three commands decide: the worker scan and the `init` template, the repository source (a local clone through `Git`, or GitHub through one new `Forge` method, `repo_root`), stack detection, the appended block, the doctor `fix` table and JSON lines, and the error type with its exit codes. `app.rs` only parses the CLI, calls `setup`, prints, and turns a `SetupError` into its exit code. `doctor()` and `Check` stay as they are.

**Tech Stack:** Rust 2024, clap 4 (derive), tokio, serde_json, toml 1.1 (all existing), `git` and `gh` CLIs, `cargo nextest`.

**Spec:** `docs/specs/2026-10-02-agent-setup-design.md` (sections 3 to 7 and 9 to 11). Section 8 (installer, landing copy block, docs sync, `llms.txt`) is planned in the landing repository: `/Users/antoinehoriot/Projects/provefab/landing/docs/superpowers/plans/2026-10-02-install-and-copy-block.md`, branch `feature/agent-setup` there, which needs Task 7 of this plan (`docs/guide/agents.md`).

## Global Constraints

- Core repo `/Users/antoinehoriot/Projects/provefab/provefab`, branch `feature/agent-setup` (checked out). Local commits only; never push, tag or deploy.
- Spec §11, verbatim: "Core: exactly one new module (`setup.rs`), no migration, no new dependency. More is a STOP." No new feature of an existing dependency either (`toml`'s `display` feature, used for escaping, is on by default). A new integration test file under `crates/provefab/tests/` is not a module (this plan adds `tests/setup.rs`). New methods on existing types (`Forge::repo_root`, three `Git` methods) are not modules (decision 1).
- Spec §11, verbatim: "Version 0.6.0." (Task 7).
- Spec §2, verbatim: "The agent never installs the binary or the service, never handles a secret, never labels an issue, never starts `provefab run`."
- Spec §2, verbatim: "Gate commands come from detection only; no override flags at creation (a repository that is not recognised is configured by hand)."
- Spec §5, verbatim: "No secret appears in any field." and "Without `--json` the output is unchanged."
- Spec §6, verbatim: "0 success; 1 a doctor check failed; 2 usage error; 3 configuration or repository already exists; 4 stack or worker not recognised; 5 `gh` or network error. Errors are one line on stderr starting with `provefab:`. Codes are documented in `--help` of the new commands and in the agent guide."
- Spec §9, verbatim: "No em-dashes; no claim that setup is automatic beyond what is described."
- Repository rules `.provefab/rules.md` apply: R1 no em-dash in user-facing text; R2 never write the names `crates/provefab/tests/no_paid_code.rs` searches for (it scans every file, this plan included) and never describe how Provefab Pro implements a feature; R3 secrets never reach errors, logs, `Debug` or output (fixed error text for `gh` failures, redacted doctor details, and a test that injects a secret-looking value); R4 published migrations are never edited (this plan adds none); R5 every behaviour change has a test that failed first and its user docs in `docs/guide/`, the README only for its quick start and tables, never a test of documentation wording; R6 no guarantee wording.
- Commit trailer: blank line then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks after every task, from the repository root: `cargo fmt` then `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`. A task is done only when all three are green.

## Decisions taken in this plan (each with its why; spec amendments in Task 7)

1. **One new `Forge` method, `repo_root(slug, read)`, and three new `Git` methods (`origin_url`, `origin_head`, `root_entries`).** Spec §10 asks for detection tests "through `gh` (FakeHub serving files)", and `FakeHub` only answers through the `Forge` port; no port method reads a file or the default branch today (`ports.rs`). One method returns everything detection needs in one value (`RepoRoot`: default branch, root names, the text of the files asked for), so the port grows by one. `Gh` implements it with `gh api repos/<slug> --jq .default_branch`, `gh api repos/<slug>/contents` and `gh api -H "Accept: application/vnd.github.raw+json" repos/<slug>/contents/<name>` (no `ref`: the contents API reads the default branch), nothing cloned. `Routed` delegates to `Gh`. Provefab Pro implements no `Forge` (checked: no `impl Forge` in the Pro repository), so nothing breaks there. Re-open trigger: a second caller needs other files than the root.
2. **`--path` reads the committed default branch of the clone, not its working tree, and the clone must be that repository.** The base is `origin/HEAD` of the clone (`git symbolic-ref refs/remotes/origin/HEAD`), else the default branch from `repo_root(slug, &[])`; the files are read at `Git::base_ref(checkout, base)` (`origin/<base>`, else `<base>`). Why: detection then sees what Provefab's tasks will start from (they start from `origin/<base>`), the same as the `gh` source, whatever branch or uncommitted files the person has. The clone's `origin` must be `github.com/<slug>` (HTTPS with or without credentials, SSH, `.git` suffix and case ignored), else exit 2: a clone of another repository would configure the wrong checks. The URL is never printed (it may hold a token, R3).
3. **`--path` is not written as `local_path`.** Spec §4: the block holds "`slug`, `label = "provefab"`, `base`, `gates`; nothing else". Provefab then keeps its own clone under `<home>/repos/<owner>/<name>`; `repos add` says so in its output and the configuration guide says to add `local_path` by hand to use your clone.
4. **Exit code 1 also covers errors outside the spec's table** (a configuration file that does not load, an I/O error). Spec §6 gives 1 to "a doctor check failed" only, but every command exits 1 on an error today (`app.rs`, `run_from`), and an unreadable or invalid file is neither a usage error nor one of 3 to 5. A missing configuration for `repos add` is a usage error (2: "run `provefab init` first"). `init --dry-run` with an existing file exits 3 like `init`. The agent guide documents "1: a doctor check failed, or another error (the message says which)".
5. **`doctor`'s exit code keeps the Jev key optional.** Spec §5 says "Exit code 0 when every check passes, 1 otherwise", but today a missing key is reported (`jev key` FAIL) and does not fail `doctor` (`app.rs`: "The Jev key is optional"), because Provefab runs without it. `--json` prints the `jev key` line with `"ok": false` and its `fix` (the spec's own example), and the exit code ignores it, with or without `--json`. The agent guide says so.
6. **`doctor --json` prints every finding as a line.** Policy warnings (`warn ...` in text) become `{"name": "warning", "ok": true, ...}` lines; a refused merge setting becomes a `merge settings` line; a configuration that is missing or does not load becomes one `configuration` line (`fix: provefab init` when missing) and exit 1, instead of an error with no line. Without `--json` the output is byte for byte today's.
7. **`fix` is computed from the check's name in `setup.rs` (`fix_for`), not stored on `Check`.** `Check { name, ok, detail }` is public and built in many places; a name table leaves `doctor()` untouched. Fixes: `gh login` → `gh auth login`; `claude login` → `provefab login claude`; `claude api key` → `provefab login claude --api-key`; `codex login`, `codex guard hook` → `provefab login codex`; `codex api login`, `codex api guard hook` → `provefab login codex --api-key`; `jev key` → `security add-generic-password -s provefab-typesafe -a provefab -w` (the key is typed at the Keychain prompt, never on a command line); `tracker <slug>` → `provefab login jira --site <site>` or `provefab login linear`; `configuration` (missing) → `provefab init`. Only failed checks get a `fix`. A missing tool (`git`, `gh`, `claude`, `codex`) gets none: installing is the person's, and the detail names the tool.
8. **JSON details are redacted with `rules::redact_credentials`.** A probe's first line (`gh auth status`, `codex login status`) is printed as the detail; spec §5 says no secret in any field. The text output stays unchanged (spec §5).
9. **A stack counts when its marker is at the root.** Markers: `Cargo.toml`; `package.json`; `pyproject.toml`, or `setup.py`/`setup.cfg` with a `tests` directory; `go.mod`. Two markers are "several stacks" (exit 4) even when one of them would give no gate; a single Node marker without a `lint`, `typecheck` or `test` script is "not recognised" (exit 4). Why: the spec's "exactly one stack" and "no script among them: not recognised" read together; guessing which of two manifests matters would pick gates the owner did not choose.
10. **npm's placeholder test script is not a gate.** `npm init` writes `"test": "echo \"Error: no test specified\" && exit 1"`; a gate from it would fail every task. A script containing `no test specified`, or empty, does not exist for detection.
11. **Ruff is configured when `pyproject.toml` has a `tool.ruff` table (any `[tool.ruff...]` section) or the root has `ruff.toml`.** `.ruff.toml` is not counted (the spec names `ruff.toml`). A `pyproject.toml` that does not parse counts as no ruff.
12. **The block is appended as text, with TOML escaping.** Strings go through `toml::Value::String(..).to_string()` (a branch name may hold `"`); the block is separated from the file by one blank line (two newlines when the file does not end with one); it is written with an append, so every earlier byte stays (spec §12 decision 4). `--dry-run` prints the block alone.
13. **The duplicate check (any case) runs before GitHub is read**, so a repeat call by an agent costs no `gh` call and exits 3.
14. **`init` creates the home directory and writes with `create_new`**, so a file created meanwhile is never overwritten (exit 3).
15. **`gh` failures are fixed text** (R3): `gh is not installed or cannot run`; `<slug> was not found on GitHub, or gh's account cannot read it` (HTTP 404 or a permanent refusal); `GitHub did not answer through gh; check `gh auth status` and the network`. `gh`'s stderr is never printed.
16. **Every error is one line.** `toml` parse errors span several lines (checked: `TOML parse error at line 2, column 3\n  |\n2 | [x ...`); `setup` joins whitespace before returning them.
17. **The exit codes are in `after_help` of `init`, `repos`, `repos add` and `doctor`** (one constant, `setup::EXIT_CODES`). Clap already exits 2 on a usage error.
18. **Dependencies are the owner's.** A task's worktree starts without `node_modules` or a virtualenv; detected Node and Python gates may need an install first. The spec forbids gate overrides at creation (owner decision 2), so `repos add` prints a note for Node and Python, and the configuration guide says to put the install command first in `gates` by hand when needed. Re-open trigger: owners ask for it (spec §11, "Overriding detected gates").

## Review Focus

1. **`--path` pointing at a clone of another repository, or at a clone whose `origin` URL carries a token**: refused with exit 2, the URL never printed. Test in Task 3 (`a_checkout_of_another_repository_is_refused_without_printing_its_url`).
2. **A file that does not end with a newline, ends in a comment, or a default branch holding a quote**: the block is escaped, the file still loads, and every earlier byte is unchanged. Tests in Task 4 (`a_repository_is_appended_and_the_rest_is_kept_byte_for_byte`, `a_branch_name_with_a_quote_is_escaped`).
3. **A `package.json` straight from `npm init`**: its placeholder `test` script is not a gate, so the repository is refused instead of configured with a gate that always fails. Test in Task 3 (the `npm init` case of `detection_per_stack_from_a_clone_and_through_github`).
4. **A loader error spanning several lines** (a `toml` parse error after the append): one stderr line, nothing written. Tests in Task 4 (`a_file_that_would_not_load_is_refused_on_one_line`) and Task 6 (every refused binary call asserts one stderr line).
5. **A doctor detail or a `gh` error holding a token**: absent from the JSON line and from the error. Tests in Task 5 (`json_lines_have_the_shape_and_no_secret`) and Task 3 (`github_errors_are_fixed_text_with_exit_code_5`).

## File Structure

| File | Responsibility |
|---|---|
| `crates/provefab/src/setup.rs` (new) | `SetupError` and exit codes, `EXIT_CODES`; `Workers`, `workers_on_path`, `init_text`, `init`; `read_root`, `origin_matches`, `gh_error`; `Stack`, `stacks`, `detect`; `valid_slug`, `repo_block`, `repos_add`; `fix_for`, `doctor_passed`, `doctor_json`, `config_check` |
| `crates/provefab/src/lib.rs` | `pub mod setup;` |
| `crates/provefab/src/forge.rs` | `RepoRoot`; `Gh::repo_root`; `Git::origin_url`, `Git::origin_head`, `Git::root_entries` |
| `crates/provefab/src/ports.rs`, `tracker.rs`, `testkit.rs` | `Forge::repo_root` and its `Gh`, `Routed` and `FakeHub` implementations |
| `crates/provefab/src/app.rs` | `init`, `repos add`, `doctor --json`, exit codes |
| `crates/provefab/tests/setup.rs` (new) | the three commands through the binary, with their exit codes |
| `docs/guide/agents.md` (new), `docs/guide/configuration.md`, `docs/guide/operations.md`, `README.md`, the spec, `Cargo.toml`, `Cargo.lock` | documentation, spec amendments, 0.6.0 |

---

### Task 1: `Forge::repo_root` and the `Git` methods a clone is read with

**Files:**
- Modify: `crates/provefab/src/forge.rs:5` (imports), after `PullRequest` (`:960-973`) for `RepoRoot`, `impl Git` after `show_file` (`:670-686`), `impl Gh` after `repo_is_public` (`:1339-1355`), tests in `mod tests` (`:1567`)
- Modify: `crates/provefab/src/ports.rs:10` (import), `trait Forge` (after `repo_is_public`), `impl Forge for Gh`
- Modify: `crates/provefab/src/tracker.rs` `impl Forge for Routed` (`:736-790`)
- Modify: `crates/provefab/src/testkit.rs:14` (re-export), `struct FakeHub` (`:220-287`), `FakeHub::new` (`:290-342`), `impl Forge for FakeHub` (`:448-610`)

**Interfaces:**
- Consumes: nothing new.
- Produces:

```rust
// forge.rs
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoRoot {
    pub default_branch: String,
    /// Names at the root of the default branch; a directory's ends with `/`.
    pub entries: std::collections::BTreeSet<String>,
    /// The text of each file asked for that is at the root.
    pub files: std::collections::BTreeMap<String, String>,
}
impl Gh { pub async fn repo_root(&self, slug: &str, read: &[&str]) -> Result<RepoRoot, ForgeError>; }
impl Git {
    pub async fn origin_url(&self, repo: &Path) -> Option<String>;
    pub async fn origin_head(&self, repo: &Path) -> Option<String>;   // "main", not "origin/main"
    pub async fn root_entries(&self, repo: &Path, rev: &str) -> Result<BTreeSet<String>, ForgeError>;
}
// ports.rs, trait Forge
fn repo_root(&self, slug: &str, read: &[&str]) -> impl Future<Output = Result<RepoRoot, ForgeError>> + Send;
// testkit.rs, FakeHub
pub repo_root: Mutex<Option<RepoRoot>>,                 // None: gh answers HTTP 404
pub repo_root_calls: Mutex<Vec<(String, Vec<String>)>>, // (slug, read)
```

- [ ] **Step 1: Write the failing tests**

In `crates/provefab/src/forge.rs`, inside `mod tests`, add:

```rust
    /// A fake `gh` that logs each call and answers by its whole argv.
    fn fake_gh_api(dir: &Path, cases: &[(&str, &str)]) -> Gh {
        let mut script = format!(
            "#!/bin/sh\necho \"$*\" >> {}\ncase \"$*\" in\n",
            dir.join("log.txt").display()
        );
        for (i, (argv, out)) in cases.iter().enumerate() {
            let file = dir.join(format!("out{i}.txt"));
            std::fs::write(&file, out).unwrap();
            script.push_str(&format!("  '{argv}') cat {} ;;\n", file.display()));
        }
        script.push_str("  *) echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;\nesac\n");
        let bin = dir.join("gh");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Gh { program: bin }
    }

    /// Agent setup spec section 4: the default branch, the root names and
    /// the files asked for that exist, through `gh api`; nothing cloned.
    #[tokio::test]
    async fn gh_reads_a_repository_root_without_cloning() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh_api(
            dir.path(),
            &[
                ("api repos/o/r --jq .default_branch", "trunk\n"),
                (
                    "api repos/o/r/contents",
                    r#"[{"name":"package.json","type":"file"},{"name":"tests","type":"dir"},{"name":"pnpm-lock.yaml","type":"file"}]"#,
                ),
                (
                    "api -H Accept: application/vnd.github.raw+json repos/o/r/contents/package.json",
                    r#"{"scripts":{"test":"vitest"}}"#,
                ),
            ],
        );
        let root = gh
            .repo_root("o/r", &["package.json", "pyproject.toml"])
            .await
            .unwrap();
        assert_eq!(root.default_branch, "trunk");
        assert_eq!(
            root.entries.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["package.json", "pnpm-lock.yaml", "tests/"]
        );
        assert_eq!(
            root.files.get("package.json").map(String::as_str),
            Some(r#"{"scripts":{"test":"vitest"}}"#)
        );
        assert_eq!(root.files.len(), 1, "pyproject.toml is not there: not read");
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert_eq!(log.lines().count(), 3, "{log}");
        assert!(!log.contains("clone"), "{log}");
    }

    /// An empty repository has no contents (GitHub answers 404): no names.
    #[tokio::test]
    async fn an_empty_repository_has_an_empty_root() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh_api(
            dir.path(),
            &[("api repos/o/r --jq .default_branch", "main")],
        );
        let root = gh.repo_root("o/r", &["package.json"]).await.unwrap();
        assert_eq!(
            (root.default_branch.as_str(), root.entries.len(), root.files.len()),
            ("main", 0, 0)
        );
        // A repository gh cannot read: the error says not found.
        let other = dir.path().join("x");
        std::fs::create_dir_all(&other).unwrap();
        let missing = fake_gh_api(&other, &[]);
        assert!(missing.repo_root("o/r", &[]).await.unwrap_err().is_not_found());
    }
```

Then, still in `mod tests`:

```rust
    /// Agent setup plan decision 2: a clone's origin, its default branch as
    /// of the clone, and the root names of a commit.
    #[tokio::test]
    async fn a_clone_tells_its_origin_default_branch_and_root() {
        use crate::testkit::git as sh;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("tests")).unwrap();
        sh(&src, &["init", "-q", "-b", "trunk"]);
        std::fs::write(src.join("go.mod"), "module x\n").unwrap();
        std::fs::write(src.join("tests/a_test.go"), "").unwrap();
        std::fs::write(src.join("we ird.txt"), "").unwrap();
        sh(&src, &["add", "-A"]);
        sh(&src, &["commit", "-q", "-m", "init"]);
        let clone = dir.path().join("clone");
        sh(
            dir.path(),
            &["clone", "-q", src.to_str().unwrap(), clone.to_str().unwrap()],
        );
        let g = git();
        assert_eq!(g.origin_head(&clone).await.as_deref(), Some("trunk"));
        assert_eq!(
            g.origin_url(&clone).await.as_deref(),
            Some(src.to_str().unwrap())
        );
        let names = g.root_entries(&clone, "origin/trunk").await.unwrap();
        assert_eq!(
            names.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["go.mod", "tests/", "we ird.txt"]
        );
        sh(&clone, &["remote", "set-head", "origin", "-d"]);
        assert_eq!(g.origin_head(&clone).await, None);
        assert_eq!(g.origin_url(&src).await, None, "no origin remote");
        assert!(g.root_entries(&clone, "origin/nope").await.is_err());
    }
```

In `crates/provefab/src/testkit.rs`, there is no test module; the `FakeHub` behaviour is pinned by Task 3's tests through `read_root`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab forge::tests::gh_reads_a_repository_root_without_cloning forge::tests::an_empty_repository_has_an_empty_root forge::tests::a_clone_tells_its_origin_default_branch_and_root`
Expected: FAIL to compile: `no method named repo_root found for struct Gh`, `no method named origin_head found for struct Git`.

- [ ] **Step 3: Write the implementation**

`crates/provefab/src/forge.rs`, imports:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
```

After `pub struct PullRequest { ... }`:

```rust
/// What setup reads of a repository (agent setup spec section 4): its
/// default branch and, on it, the names at the root and the text of the
/// files asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoRoot {
    pub default_branch: String,
    /// Names at the root; a directory's ends with `/`.
    pub entries: BTreeSet<String>,
    /// The text of each file asked for that is at the root.
    pub files: BTreeMap<String, String>,
}
```

In `impl Git`, after `show_file`:

```rust
    /// The URL of the `origin` remote, or `None` without one.
    pub async fn origin_url(&self, repo: &Path) -> Option<String> {
        self.git(repo, &["remote", "get-url", "origin"])
            .await
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
    }

    /// The branch `origin/HEAD` names (the remote's default branch as of the
    /// clone or the last `git remote set-head`), without `origin/`.
    pub async fn origin_head(&self, repo: &Path) -> Option<String> {
        let out = self
            .git(
                repo,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .await
            .ok()?;
        out.trim()
            .strip_prefix("origin/")
            .filter(|b| !b.is_empty())
            .map(str::to_string)
    }

    /// The names at the root of `rev`; a directory's ends with `/`.
    pub async fn root_entries(&self, repo: &Path, rev: &str) -> Result<BTreeSet<String>, ForgeError> {
        let out = self.git(repo, &["ls-tree", "-z", rev]).await?;
        Ok(out
            .split('\0')
            .filter_map(|line| {
                let (meta, name) = line.split_once('\t')?;
                Some(if meta.split(' ').nth(1) == Some("tree") {
                    format!("{name}/")
                } else {
                    name.to_string()
                })
            })
            .collect())
    }
```

In `impl Gh`, after `repo_is_public`:

```rust
    /// The default branch of `slug` and its root on that branch: the names
    /// there and the text of each file of `read` that exists. Read through
    /// the API, nothing cloned (agent setup spec section 4). The contents
    /// API reads the default branch when given no `ref`.
    pub async fn repo_root(&self, slug: &str, read: &[&str]) -> Result<RepoRoot, ForgeError> {
        let repo = format!("repos/{slug}");
        let default_branch = self
            .gh(&["api", &repo, "--jq", ".default_branch"], None)
            .await?
            .trim()
            .to_string();
        if default_branch.is_empty() || default_branch == "null" {
            return Err(ForgeError::Parse(
                "gh api repos".into(),
                "no default branch".into(),
            ));
        }
        let contents = format!("{repo}/contents");
        let listing = match self.gh(&["api", &contents], None).await {
            Ok(out) => out,
            // An empty repository has no contents: nothing to detect.
            Err(e) if e.is_not_found() => "[]".to_string(),
            Err(e) => return Err(e),
        };
        let v = self.json("gh api contents", &listing)?;
        let mut entries = BTreeSet::new();
        for e in v.as_array().into_iter().flatten() {
            let Some(name) = e.get("name").and_then(Value::as_str) else {
                continue;
            };
            if e.get("type").and_then(Value::as_str) == Some("dir") {
                entries.insert(format!("{name}/"));
            } else {
                entries.insert(name.to_string());
            }
        }
        let mut files = BTreeMap::new();
        for name in read {
            if !entries.contains(*name) {
                continue;
            }
            let text = self
                .gh(
                    &[
                        "api",
                        "-H",
                        "Accept: application/vnd.github.raw+json",
                        &format!("{contents}/{name}"),
                    ],
                    None,
                )
                .await?;
            files.insert(name.to_string(), text);
        }
        Ok(RepoRoot {
            default_branch,
            entries,
            files,
        })
    }
```

`crates/provefab/src/ports.rs`: import `RepoRoot` (`use crate::forge::{Comment, ForgeError, Gh, Issue, PrStatus, PullRequest, RepoRoot};`); in `trait Forge`, after `repo_is_public`:

```rust
    /// The default branch and, on it, the repository's root: names and the
    /// text of the files of `read` that exist (agent setup spec section 4).
    fn repo_root(
        &self,
        slug: &str,
        read: &[&str],
    ) -> impl Future<Output = Result<RepoRoot, ForgeError>> + Send;
```

and in `impl Forge for Gh`:

```rust
    async fn repo_root(&self, slug: &str, read: &[&str]) -> Result<RepoRoot, ForgeError> {
        Gh::repo_root(self, slug, read).await
    }
```

`crates/provefab/src/tracker.rs`, `impl Forge for Routed` (add `RepoRoot` to its `crate::forge` import):

```rust
    async fn repo_root(&self, slug: &str, read: &[&str]) -> Result<RepoRoot, ForgeError> {
        Forge::repo_root(&self.gh, slug, read).await
    }
```

`crates/provefab/src/testkit.rs`: re-export `pub use crate::forge::{Comment, ForgeError, Git, Issue, PullRequest, RepoRoot};`; in `struct FakeHub`, after `pr_status_down`:

```rust
    /// What `repo_root` answers (its files kept to those asked for); `None`
    /// answers as gh does for a repository it cannot read.
    pub repo_root: Mutex<Option<RepoRoot>>,
    /// Every (slug, read) `repo_root` was asked for.
    pub repo_root_calls: Mutex<Vec<(String, Vec<String>)>>,
```

in `FakeHub::new`, after `pr_status_down: Default::default(),`:

```rust
            repo_root: Mutex::new(None),
            repo_root_calls: Mutex::new(Vec::new()),
```

in `impl Forge for FakeHub`, after `repo_is_public`:

```rust
    async fn repo_root(&self, slug: &str, read: &[&str]) -> Result<RepoRoot, ForgeError> {
        self.repo_root_calls.lock().unwrap().push((
            slug.to_string(),
            read.iter().map(|s| s.to_string()).collect(),
        ));
        let Some(mut root) = self.repo_root.lock().unwrap().clone() else {
            return Err(ForgeError::Failed {
                program: "gh".into(),
                args: format!("api repos/{slug}"),
                code: Some(1),
                stderr: "gh: Not Found (HTTP 404) token=ghp_SECRET0123456789abcdef".into(),
            });
        };
        root.files.retain(|name, _| read.contains(&name.as_str()));
        Ok(root)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab forge::tests::gh_reads_a_repository_root_without_cloning forge::tests::an_empty_repository_has_an_empty_root forge::tests::a_clone_tells_its_origin_default_branch_and_root ports::tests`
Expected: PASS.

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/forge.rs crates/provefab/src/ports.rs crates/provefab/src/tracker.rs crates/provefab/src/testkit.rs
git commit -m "feat: read a repository's root through gh or a clone

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `setup.rs`: errors, exit codes and `provefab init`

**Files:**
- Create: `crates/provefab/src/setup.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod setup;` after `pub mod service;`)

**Interfaces:**
- Consumes: `Config::from_toml_str` (`config.rs`), `Paths::{home, config}` (`paths.rs`).
- Produces:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("{0}")] Usage(String),          // exit 2
    #[error("{0}")] Exists(String),         // exit 3
    #[error("{0}")] NotRecognised(String),  // exit 4
    #[error("{0}")] Gh(String),             // exit 5
    #[error("{0}")] Other(String),          // exit 1
}
impl SetupError { pub fn code(&self) -> u8; }
pub const EXIT_CODES: &str;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Workers { pub claude: bool, pub codex: bool }
pub fn workers_on_path(path: &std::ffi::OsStr) -> Workers;
pub fn init_text(w: Workers) -> Result<String, SetupError>;
/// What to print on success: the file (dry run) or what was written.
pub fn init(paths: &Paths, path_var: &std::ffi::OsStr, dry_run: bool) -> Result<String, SetupError>;
pub(crate) fn one_line(s: &str) -> String;
```

- [ ] **Step 1: Write the failing tests**

Create `crates/provefab/src/setup.rs` holding only the module doc and the tests (the tests reference items Step 3 adds):

```rust
//! What an agent runs to configure Provefab (agent setup spec): `init`,
//! `repos add`, the JSON lines of `doctor --json`, and their exit codes.

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn tool(dir: &Path, name: &str) {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn example() -> Config {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../provefab.example.toml"
        ))
        .unwrap();
        Config::from_toml_str(&text).unwrap()
    }

    #[test]
    fn each_error_has_its_exit_code() {
        let codes: Vec<u8> = [
            SetupError::Other("x".into()),
            SetupError::Usage("x".into()),
            SetupError::Exists("x".into()),
            SetupError::NotRecognised("x".into()),
            SetupError::Gh("x".into()),
        ]
        .iter()
        .map(SetupError::code)
        .collect();
        assert_eq!(codes, vec![1, 2, 3, 4, 5]);
        assert!(!EXIT_CODES.contains('\u{2014}'));
        assert_eq!(one_line("a\n  |\n2 | [x\n"), "a | 2 | [x");
    }

    /// Spec section 3: `claude` gives `claude-sonnet` and `claude-opus`,
    /// `codex` gives `codex-gpt`, the same entries as the example file; a
    /// file that is not executable does not count.
    #[test]
    fn the_catalog_comes_from_the_worker_clis_on_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        tool(a.path(), "claude");
        tool(b.path(), "codex");
        std::fs::write(b.path().join("claude"), "not a program").unwrap();
        let both = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert_eq!(
            workers_on_path(&both),
            Workers {
                claude: true,
                codex: true
            }
        );
        let only_b = std::env::join_paths([b.path()]).unwrap();
        assert_eq!(
            workers_on_path(&only_b),
            Workers {
                claude: false,
                codex: true
            }
        );
        let example = example();
        for (w, ids) in [
            (
                Workers {
                    claude: true,
                    codex: true,
                },
                vec!["claude-sonnet", "codex-gpt", "claude-opus"],
            ),
            (
                Workers {
                    claude: true,
                    codex: false,
                },
                vec!["claude-sonnet", "claude-opus"],
            ),
            (
                Workers {
                    claude: false,
                    codex: true,
                },
                vec!["codex-gpt"],
            ),
        ] {
            let config = Config::from_toml_str(&init_text(w).unwrap()).unwrap();
            assert_eq!(
                config.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                ids
            );
            for m in &config.models {
                assert_eq!(
                    Some(m),
                    example.models.iter().find(|e| e.id == m.id),
                    "{}",
                    m.id
                );
            }
            assert_eq!(config.jev, example.jev);
            assert!(config.repos.is_empty(), "no repository is added");
        }
    }

    /// Spec section 3: written once into a home that may not exist yet,
    /// loadable, `--dry-run` writes nothing, an existing file is never
    /// changed (exit 3, with or without `--dry-run`).
    #[test]
    fn init_writes_once_and_never_overwrites() {
        let bins = tempfile::tempdir().unwrap();
        tool(bins.path(), "claude");
        let path = std::env::join_paths([bins.path()]).unwrap();
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(&home.path().join("fresh"));
        let shown = init(&paths, &path, true).unwrap();
        assert!(!paths.config().exists(), "--dry-run writes nothing");
        assert!(shown.contains("id = \"claude-opus\""), "{shown}");
        let said = init(&paths, &path, false).unwrap();
        assert!(said.contains("models: claude-sonnet, claude-opus"), "{said}");
        assert!(said.contains("next: provefab repos add <owner/name>"), "{said}");
        let written = std::fs::read_to_string(paths.config()).unwrap();
        assert_eq!(written, shown);
        Config::from_toml_str(&written).unwrap();
        std::fs::write(paths.config(), "# mine\n").unwrap();
        for dry_run in [false, true] {
            let e = init(&paths, &path, dry_run).unwrap_err();
            assert_eq!(e.code(), 3, "{e}");
            assert!(e.to_string().contains("already exists"), "{e}");
        }
        assert_eq!(std::fs::read_to_string(paths.config()).unwrap(), "# mine\n");
    }

    #[test]
    fn init_without_a_worker_cli_writes_nothing() {
        let empty = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let path = std::env::join_paths([empty.path()]).unwrap();
        let e = init(&paths, &path, false).unwrap_err();
        assert_eq!(e.code(), 4);
        assert!(
            e.to_string()
                .contains("install Claude Code (`claude`) or the Codex CLI (`codex`)"),
            "{e}"
        );
        assert!(!paths.config().exists());
    }
}
```

In `crates/provefab/src/lib.rs`, after `pub mod service;`, add `pub mod setup;`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: FAIL to compile: `cannot find type SetupError in this scope`, `cannot find function init_text`.

- [ ] **Step 3: Write the implementation**

Above `#[cfg(test)]` in `crates/provefab/src/setup.rs`:

```rust
use std::ffi::OsStr;
use std::io::Write as _;

use crate::config::Config;
use crate::paths::Paths;

/// The exit codes of `init`, `repos add` and `doctor` (spec section 6),
/// shown by their `--help`.
pub const EXIT_CODES: &str = "Exit codes: 0 success; 1 a doctor check failed, or another error; 2 usage error (no configuration yet: run `provefab init`); 3 the configuration or the repository already exists; 4 stack or worker not recognised; 5 gh or network error. Errors are one line on stderr starting with `provefab:`.";

/// Why a setup command stopped; each kind has its exit code (spec section 6).
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Exists(String),
    #[error("{0}")]
    NotRecognised(String),
    #[error("{0}")]
    Gh(String),
    /// A file that does not load, an I/O error (plan decision 4).
    #[error("{0}")]
    Other(String),
}

impl SetupError {
    pub fn code(&self) -> u8 {
        match self {
            SetupError::Other(_) => 1,
            SetupError::Usage(_) => 2,
            SetupError::Exists(_) => 3,
            SetupError::NotRecognised(_) => 4,
            SetupError::Gh(_) => 5,
        }
    }
}

/// `s` on one line: errors are one stderr line (plan decision 16).
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The worker CLIs found on `PATH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Workers {
    pub claude: bool,
    pub codex: bool,
}

fn on_path(path: &OsStr, program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path).any(|dir| {
        std::fs::metadata(dir.join(program))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

pub fn workers_on_path(path: &OsStr) -> Workers {
    Workers {
        claude: on_path(path, "claude"),
        codex: on_path(path, "codex"),
    }
}

const NO_WORKER: &str = "no worker CLI on PATH: install Claude Code (`claude`) or the Codex CLI (`codex`), then run `provefab init` again; nothing written";

const HEADER: &str = r#"# Provefab configuration, written by `provefab init`.
# Full reference: https://provefab.com/docs/configuration/
# No secret here: sign-ins stay in Provefab's directories, keys in the macOS Keychain.

[jev]
model = "jev-1.13.0"            # full, pinned version; never "jev-latest"
underspecified_threshold = 0.7  # above this, Provefab asks a question instead of coding
loop_threshold = 0.8            # above this, an agent going in circles is stopped

# The catalog: the models of the worker CLIs found on PATH.
"#;

// The example file's entries (spec section 3), in its order.
const CLAUDE_SONNET: &str = r#"
[[models]]
id = "claude-sonnet"
worker = "claude-code"
model = "sonnet"
tier = "standard"
"#;

const CODEX_GPT: &str = r#"
[[models]]
id = "codex-gpt"
worker = "codex"
model = "gpt-5.5"
tier = "standard"
"#;

const CLAUDE_OPUS: &str = r#"
[[models]]
id = "claude-opus"
worker = "claude-code"
model = "opus"
tier = "frontier"
"#;

const FOOTER: &str = "\n# Repositories: add each one with `provefab repos add <owner/name>`.\n";

/// The file `init` writes for these workers, checked with the loader
/// `provefab run` uses.
pub fn init_text(w: Workers) -> Result<String, SetupError> {
    if !w.claude && !w.codex {
        return Err(SetupError::NotRecognised(NO_WORKER.into()));
    }
    let mut text = HEADER.to_string();
    if w.claude {
        text.push_str(CLAUDE_SONNET);
    }
    if w.codex {
        text.push_str(CODEX_GPT);
    }
    if w.claude {
        text.push_str(CLAUDE_OPUS);
    }
    text.push_str(FOOTER);
    Config::from_toml_str(&text).map_err(|e| SetupError::Other(one_line(&e.to_string())))?;
    Ok(text)
}

/// `provefab init` (spec section 3): the file on a dry run, else what was
/// written. An existing file is never touched (plan decision 14).
pub fn init(paths: &Paths, path_var: &OsStr, dry_run: bool) -> Result<String, SetupError> {
    let file = paths.config();
    let exists = || {
        SetupError::Exists(format!(
            "{} already exists; nothing changed (keep it, or edit it by hand)",
            file.display()
        ))
    };
    if file.exists() {
        return Err(exists());
    }
    let text = init_text(workers_on_path(path_var))?;
    if dry_run {
        return Ok(text);
    }
    std::fs::create_dir_all(&paths.home).map_err(|e| {
        SetupError::Other(format!("cannot create {}: {e}", paths.home.display()))
    })?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => exists(),
            _ => SetupError::Other(format!("cannot write {}: {e}", file.display())),
        })?;
    f.write_all(text.as_bytes())
        .map_err(|e| SetupError::Other(format!("cannot write {}: {e}", file.display())))?;
    let ids: Vec<String> = Config::from_toml_str(&text)
        .map(|c| c.models.into_iter().map(|m| m.id).collect())
        .unwrap_or_default();
    Ok(format!(
        "wrote {}\nmodels: {}\nnext: provefab repos add <owner/name>\n",
        file.display(),
        ids.join(", ")
    ))
}
```

`std::path::Path` is imported by the tests module only in this task; Task 3 adds it at module level.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: PASS (4 tests).

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/setup.rs crates/provefab/src/lib.rs
git commit -m "feat: provefab init writes the configuration from the workers on PATH

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Reading a repository and detecting its stack

**Files:**
- Modify: `crates/provefab/src/setup.rs` (after `init`, and its tests)

**Interfaces:**
- Consumes: `RepoRoot`, `Forge::repo_root`, `Git::{origin_url, origin_head, root_entries, base_ref, show_file}`, `ForgeError::{is_not_found, is_permanent}` (Task 1); `SetupError`, `one_line` (Task 2).
- Produces:

```rust
pub async fn read_root(forge: &impl Forge, git: &Git, slug: &str, checkout: Option<&Path>) -> Result<RepoRoot, SetupError>;
pub fn origin_matches(url: &str, slug: &str) -> bool;
fn gh_error(slug: &str, e: &ForgeError) -> SetupError;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack { Rust, Node, Python, Go }
impl Stack { pub fn name(self) -> &'static str; }
pub fn stacks(root: &RepoRoot) -> Vec<Stack>;
/// The gates of the one stack at the root, or why there is none.
pub fn detect(root: &RepoRoot, slug: &str) -> Result<Vec<String>, SetupError>;
pub const BY_HAND: &str;
```

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `setup.rs`, add:

```rust
    use crate::forge::{ForgeError, Git, RepoRoot};
    use crate::testkit::FakeHub;

    fn git() -> Git {
        Git {
            program: "git".into(),
        }
    }

    /// The root GitHub would list for `files` (paths with `/` are in a
    /// directory), on `branch`.
    fn root_of(files: &[(&str, &str)], branch: &str) -> RepoRoot {
        let mut root = RepoRoot {
            default_branch: branch.into(),
            ..RepoRoot::default()
        };
        for (path, text) in files {
            match path.split_once('/') {
                Some((dir, _)) => {
                    root.entries.insert(format!("{dir}/"));
                }
                None => {
                    root.entries.insert(path.to_string());
                    root.files.insert(path.to_string(), text.to_string());
                }
            }
        }
        root
    }

    /// `files` twice: committed on `main` in a repository cloned so that its
    /// origin is github.com/o/r with `origin/HEAD` set, and served by a
    /// FakeHub as GitHub's root.
    struct Both {
        _dir: tempfile::TempDir,
        checkout: std::path::PathBuf,
        hub: FakeHub,
    }

    fn both(files: &[(&str, &str)]) -> Both {
        use crate::testkit::git as sh;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        sh(&src, &["init", "-q", "-b", "main"]);
        for (path, text) in files {
            let p = src.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        sh(&src, &["add", "-A"]);
        sh(&src, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let checkout = dir.path().join("checkout");
        sh(
            dir.path(),
            &["clone", "-q", src.to_str().unwrap(), checkout.to_str().unwrap()],
        );
        sh(
            &checkout,
            &["remote", "set-url", "origin", "https://github.com/o/r.git"],
        );
        let hub = FakeHub::new("x");
        *hub.repo_root.lock().unwrap() = Some(root_of(files, "main"));
        Both {
            _dir: dir,
            checkout,
            hub,
        }
    }

    const NPM_INIT: &str = r#"{"name":"x","scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#;

    /// Spec section 10: each stack from fixture files, read from a clone
    /// (`--path`) and through GitHub (FakeHub serving the same files).
    #[tokio::test]
    async fn detection_per_stack_from_a_clone_and_through_github() {
        type Want = Result<Vec<&'static str>, &'static str>;
        let cases: Vec<(&str, Vec<(&str, &str)>, Want)> = vec![
            (
                "rust",
                vec![("Cargo.toml", "[package]\nname = \"x\"\n")],
                Ok(vec![
                    "cargo fmt -- --check",
                    "cargo clippy --all-targets -- -D warnings",
                    "cargo test",
                ]),
            ),
            (
                "pnpm",
                vec![
                    (
                        "package.json",
                        r#"{"scripts":{"test":"vitest run","typecheck":"tsc","lint":"eslint ."}}"#,
                    ),
                    ("pnpm-lock.yaml", ""),
                ],
                Ok(vec!["pnpm run lint", "pnpm run typecheck", "pnpm test"]),
            ),
            (
                "yarn",
                vec![
                    ("package.json", r#"{"scripts":{"test":"jest"}}"#),
                    ("yarn.lock", ""),
                ],
                Ok(vec!["yarn test"]),
            ),
            (
                "npm with scripts",
                vec![(
                    "package.json",
                    r#"{"scripts":{"lint":"eslint .","test":"node --test"}}"#,
                )],
                Ok(vec!["npm run lint", "npm test"]),
            ),
            (
                "npm without scripts",
                vec![("package.json", r#"{"name":"x"}"#)],
                Err("package.json has no lint, typecheck or test script"),
            ),
            (
                "npm init",
                vec![("package.json", NPM_INIT)],
                Err("package.json has no lint, typecheck or test script"),
            ),
            (
                "python with ruff",
                vec![(
                    "pyproject.toml",
                    "[project]\nname = \"x\"\n\n[tool.ruff.lint]\nselect = [\"E\"]\n",
                )],
                Ok(vec!["ruff check .", "pytest"]),
            ),
            (
                "python without ruff",
                vec![("pyproject.toml", "[project]\nname = \"x\"\n")],
                Ok(vec!["pytest"]),
            ),
            (
                "setup.py with tests and ruff.toml",
                vec![
                    ("setup.py", ""),
                    ("ruff.toml", "line-length = 100\n"),
                    ("tests/test_a.py", ""),
                ],
                Ok(vec!["ruff check .", "pytest"]),
            ),
            (
                "setup.py without tests",
                vec![("setup.py", "")],
                Err("no Rust, Node, Python or Go project at the root of o/r on main"),
            ),
            (
                "go",
                vec![("go.mod", "module x\n")],
                Ok(vec!["go vet ./...", "go test ./..."]),
            ),
            (
                "none",
                vec![("README.md", "# x\n")],
                Err("no Rust, Node, Python or Go project at the root of o/r on main"),
            ),
            (
                "several",
                vec![
                    ("Cargo.toml", ""),
                    ("package.json", r#"{"scripts":{"test":"jest"}}"#),
                ],
                Err("several stacks at the root of o/r (Rust, Node)"),
            ),
        ];
        for (name, files, want) in cases {
            let b = both(&files);
            for (how, checkout) in [("path", Some(b.checkout.as_path())), ("gh", None)] {
                let root = read_root(&b.hub, &git(), "o/r", checkout)
                    .await
                    .unwrap_or_else(|e| panic!("{name} {how}: {e}"));
                assert_eq!(root.default_branch, "main", "{name} {how}");
                match (detect(&root, "o/r"), &want) {
                    (Ok(got), Ok(w)) => assert_eq!(
                        got.iter().map(String::as_str).collect::<Vec<_>>(),
                        *w,
                        "{name} {how}"
                    ),
                    (Err(e), Err(w)) => {
                        assert_eq!(e.code(), 4, "{name} {how}");
                        assert!(e.to_string().contains(w), "{name} {how}: {e}");
                        assert!(e.to_string().contains(BY_HAND), "{name} {how}: {e}");
                    }
                    (got, _) => panic!("{name} {how}: {got:?}"),
                }
            }
            assert_eq!(
                b.hub.repo_root_calls.lock().unwrap().len(),
                1,
                "{name}: a clone with origin/HEAD never asks GitHub"
            );
        }
    }

    /// Plan decision 2: without `origin/HEAD`, the default branch comes
    /// from GitHub, and only the branch (no file) is asked for.
    #[tokio::test]
    async fn a_clone_without_origin_head_asks_github_for_the_default_branch() {
        let b = both(&[("go.mod", "module x\n")]);
        crate::testkit::git(&b.checkout, &["remote", "set-head", "origin", "-d"]);
        let root = read_root(&b.hub, &git(), "o/r", Some(&b.checkout))
            .await
            .unwrap();
        assert_eq!(root.default_branch, "main");
        assert!(root.entries.contains("go.mod"));
        let calls = b.hub.repo_root_calls.lock().unwrap().clone();
        assert_eq!(calls, vec![("o/r".to_string(), Vec::<String>::new())]);
    }

    #[test]
    fn origin_urls_of_the_repository_in_any_form() {
        for url in [
            "https://github.com/o/r.git",
            "https://github.com/O/R",
            "https://github.com/o/r/",
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
            "https://x-access-token:ghp_abcdefghijklmnopqrstuvwxyz0123@github.com/o/r.git",
        ] {
            assert!(origin_matches(url, "o/r"), "{url}");
        }
        for url in [
            "https://github.com/o/r2.git",
            "https://gitlab.com/o/r.git",
            "https://github.com/x/o/r",
            "https://evil.example/@github.com/o/r",
            "/tmp/r",
            "git@github.com:o/r.git.bak",
        ] {
            assert!(!origin_matches(url, "o/r"), "{url}");
        }
    }

    /// Review Focus 1: a clone of another repository configures nothing,
    /// and its URL (which can hold a token) is never printed.
    #[tokio::test]
    async fn a_checkout_of_another_repository_is_refused_without_printing_its_url() {
        let b = both(&[("Cargo.toml", "")]);
        crate::testkit::git(
            &b.checkout,
            &[
                "remote",
                "set-url",
                "origin",
                "https://x-access-token:ghp_abcdefghijklmnopqrstuvwxyz0123@github.com/other/repo.git",
            ],
        );
        let e = read_root(&b.hub, &git(), "o/r", Some(&b.checkout))
            .await
            .unwrap_err();
        assert_eq!(e.code(), 2);
        let said = e.to_string();
        assert!(said.contains("its origin is not github.com/o/r"), "{said}");
        assert!(!said.contains("ghp_") && !said.contains("other/repo"), "{said}");
        let plain = tempfile::tempdir().unwrap();
        let e = read_root(&b.hub, &git(), "o/r", Some(plain.path()))
            .await
            .unwrap_err();
        assert_eq!(e.code(), 2);
        assert!(e.to_string().contains("not a git checkout"), "{e}");
    }

    /// Review Focus 5: gh's stderr (here with a token) never reaches the
    /// message; every gh failure exits 5.
    #[tokio::test]
    async fn github_errors_are_fixed_text_with_exit_code_5() {
        let hub = FakeHub::new("x");
        let e = read_root(&hub, &git(), "o/r", None).await.unwrap_err();
        assert_eq!(
            (e.code(), e.to_string().as_str()),
            (5, "o/r was not found on GitHub, or gh's account cannot read it")
        );
        let spawn = ForgeError::Spawn {
            program: "gh".into(),
            args: "api".into(),
            message: "No such file or directory".into(),
        };
        assert_eq!(gh_error("o/r", &spawn).to_string(), "gh is not installed or cannot run");
        let slow = ForgeError::Timeout {
            program: "gh".into(),
            args: "api".into(),
            secs: 600,
        };
        let e = gh_error("o/r", &slow);
        assert_eq!(
            (e.code(), e.to_string().as_str()),
            (
                5,
                "GitHub did not answer through gh; check `gh auth status` and the network"
            )
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: FAIL to compile: `cannot find function read_root`, `cannot find function detect`.

- [ ] **Step 3: Write the implementation**

Imports at the top of `setup.rs` become:

```rust
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::forge::{ForgeError, Git, RepoRoot};
use crate::paths::Paths;
use crate::ports::Forge;
```

After `init`:

```rust
/// Where a person writes a block detection cannot (spec section 4).
pub const BY_HAND: &str =
    "write its [[repos]] block by hand (see https://provefab.com/docs/configuration/)";

/// The files detection reads besides names.
const READ: &[&str] = &["package.json", "pyproject.toml"];

/// Whether `url` (an `origin` remote) is github.com/`slug`, over HTTPS (with
/// or without credentials) or SSH, any case, with or without `.git`.
pub fn origin_matches(url: &str, slug: &str) -> bool {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u).to_ascii_lowercase();
    let want = slug.to_ascii_lowercase();
    let plain = [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .any(|p| u.strip_prefix(p) == Some(want.as_str()));
    let with_credentials = u.split_once("@github.com/").is_some_and(|(user, rest)| {
        rest == want
            && user
                .strip_prefix("https://")
                .is_some_and(|info| !info.contains('/'))
    });
    plain || with_credentials
}

/// A gh failure as fixed text (plan decision 15, R3).
fn gh_error(slug: &str, e: &ForgeError) -> SetupError {
    SetupError::Gh(match e {
        ForgeError::Spawn { .. } => "gh is not installed or cannot run".to_string(),
        e if e.is_not_found() || e.is_permanent() => {
            format!("{slug} was not found on GitHub, or gh's account cannot read it")
        }
        _ => "GitHub did not answer through gh; check `gh auth status` and the network"
            .to_string(),
    })
}

/// The repository's root on its default branch: from the clone at
/// `checkout` (which must be that repository, plan decision 2), else
/// through GitHub without cloning.
pub async fn read_root(
    forge: &impl Forge,
    git: &Git,
    slug: &str,
    checkout: Option<&Path>,
) -> Result<RepoRoot, SetupError> {
    let Some(dir) = checkout else {
        return forge
            .repo_root(slug, READ)
            .await
            .map_err(|e| gh_error(slug, &e));
    };
    let shown = dir.display();
    if !dir.join(".git").exists() {
        return Err(SetupError::Usage(format!(
            "--path {shown}: not a git checkout"
        )));
    }
    // The URL is never printed: it can hold a token (R3).
    if !git
        .origin_url(dir)
        .await
        .is_some_and(|u| origin_matches(&u, slug))
    {
        return Err(SetupError::Usage(format!(
            "--path {shown}: its origin is not github.com/{slug}"
        )));
    }
    let default_branch = match git.origin_head(dir).await {
        Some(b) => b,
        None => {
            forge
                .repo_root(slug, &[])
                .await
                .map_err(|e| gh_error(slug, &e))?
                .default_branch
        }
    };
    let rev = git.base_ref(dir, &default_branch).await;
    let entries = git.root_entries(dir, &rev).await.map_err(|_| {
        SetupError::Usage(format!(
            "--path {shown}: cannot read branch {default_branch}; run `git -C {shown} fetch origin` first"
        ))
    })?;
    let mut files = std::collections::BTreeMap::new();
    for name in READ {
        if entries.contains(*name)
            && let Ok(Some(text)) = git.show_file(dir, &rev, name).await
        {
            files.insert(name.to_string(), text);
        }
    }
    Ok(RepoRoot {
        default_branch,
        entries,
        files,
    })
}

/// A stack Provefab knows the checks of (spec section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    Rust,
    Node,
    Python,
    Go,
}

impl Stack {
    pub fn name(self) -> &'static str {
        match self {
            Stack::Rust => "Rust",
            Stack::Node => "Node",
            Stack::Python => "Python",
            Stack::Go => "Go",
        }
    }
}

/// Every stack whose marker is at the root (plan decision 9).
pub fn stacks(root: &RepoRoot) -> Vec<Stack> {
    let has = |n: &str| root.entries.contains(n);
    let mut out = Vec::new();
    if has("Cargo.toml") {
        out.push(Stack::Rust);
    }
    if has("package.json") {
        out.push(Stack::Node);
    }
    if has("pyproject.toml") || ((has("setup.py") || has("setup.cfg")) && has("tests/")) {
        out.push(Stack::Python);
    }
    if has("go.mod") {
        out.push(Stack::Go);
    }
    out
}

/// The gates of the one stack at the root (spec section 4), or why there
/// is none (exit 4).
pub fn detect(root: &RepoRoot, slug: &str) -> Result<Vec<String>, SetupError> {
    let found = stacks(root);
    let stack = match found.as_slice() {
        [one] => *one,
        [] => {
            return Err(SetupError::NotRecognised(format!(
                "no Rust, Node, Python or Go project at the root of {slug} on {}: {BY_HAND}",
                root.default_branch
            )));
        }
        many => {
            let names: Vec<&str> = many.iter().map(|s| s.name()).collect();
            return Err(SetupError::NotRecognised(format!(
                "several stacks at the root of {slug} ({}): {BY_HAND}",
                names.join(", ")
            )));
        }
    };
    let gates: Vec<&str> = match stack {
        Stack::Rust => vec![
            "cargo fmt -- --check",
            "cargo clippy --all-targets -- -D warnings",
            "cargo test",
        ],
        Stack::Go => vec!["go vet ./...", "go test ./..."],
        Stack::Python => {
            let ruff = root.entries.contains("ruff.toml")
                || root
                    .files
                    .get("pyproject.toml")
                    .and_then(|t| t.parse::<toml::Table>().ok())
                    .is_some_and(|t| {
                        t.get("tool")
                            .and_then(|tool| tool.get("ruff"))
                            .is_some()
                    });
            if ruff {
                vec!["ruff check .", "pytest"]
            } else {
                vec!["pytest"]
            }
        }
        Stack::Node => return node_gates(root, slug),
    };
    Ok(gates.into_iter().map(str::to_string).collect())
}

/// `lint`, `typecheck` and `test` scripts that exist, run by the package
/// manager of the lock file (spec section 4, plan decision 10).
fn node_gates(root: &RepoRoot, slug: &str) -> Result<Vec<String>, SetupError> {
    let has = |n: &str| root.entries.contains(n);
    let pm = if has("pnpm-lock.yaml") {
        "pnpm"
    } else if has("yarn.lock") {
        "yarn"
    } else {
        "npm"
    };
    let text = root.files.get("package.json").map_or("{}", String::as_str);
    let manifest: Value = serde_json::from_str(text).map_err(|_| {
        SetupError::NotRecognised(format!("{slug}: package.json does not parse: {BY_HAND}"))
    })?;
    let exists = |name: &str| {
        manifest
            .get("scripts")
            .and_then(|s| s.get(name))
            .and_then(Value::as_str)
            .is_some_and(|cmd| !cmd.trim().is_empty() && !cmd.contains("no test specified"))
    };
    let mut gates = Vec::new();
    if exists("lint") {
        gates.push(format!("{pm} run lint"));
    }
    if exists("typecheck") {
        gates.push(format!("{pm} run typecheck"));
    }
    if exists("test") {
        gates.push(format!("{pm} test"));
    }
    if gates.is_empty() {
        return Err(SetupError::NotRecognised(format!(
            "{slug}: package.json has no lint, typecheck or test script: {BY_HAND}"
        )));
    }
    Ok(gates)
}
```

`one_line` is used from Task 4 on in this file; keep it `pub(crate)`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: PASS.

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/setup.rs
git commit -m "feat: detect a repository's stack from a clone or through gh

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `provefab repos add`

**Files:**
- Modify: `crates/provefab/src/setup.rs` (after `node_gates`, and its tests)

**Interfaces:**
- Consumes: `read_root`, `detect` (Task 3); `SetupError`, `one_line` (Task 2); `Config::from_toml_str`, `RepoConfig::slug`.
- Produces:

```rust
pub fn valid_slug(slug: &str) -> bool;
/// One `[[repos]]` block: slug, label, base, gates (spec section 4).
pub fn repo_block(slug: &str, base: &str, gates: &[String]) -> String;
/// What to print on success: the block (dry run) or what was added.
pub async fn repos_add(paths: &Paths, forge: &impl Forge, git: &Git, slug: &str, checkout: Option<&Path>, dry_run: bool) -> Result<String, SetupError>;
```

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `setup.rs`, add:

```rust
    /// A file in a fresh home: comments, an existing repository, and no
    /// newline at the end.
    const MINE: &str = "# my notes\n[jev]\nmodel = \"jev-1.13.0\"\n\n[[models]]\nid = \"c\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"   # keep me\n\n[[repos]]\nslug = \"Acme/Api\"\ngates = [\"make\"]\n# the end, no newline";

    fn home_with(text: &str) -> (tempfile::TempDir, Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        std::fs::write(paths.config(), text).unwrap();
        (home, paths)
    }

    fn go_hub(branch: &str) -> FakeHub {
        let hub = FakeHub::new("x");
        *hub.repo_root.lock().unwrap() = Some(root_of(&[("go.mod", "module x\n")], branch));
        hub
    }

    /// Spec section 4 and Review Focus 2: one block at the end, every
    /// earlier byte kept, the whole file loads.
    #[tokio::test]
    async fn a_repository_is_appended_and_the_rest_is_kept_byte_for_byte() {
        let (_h, paths) = home_with(MINE);
        let said = repos_add(&paths, &go_hub("trunk"), &git(), "o/r", None, false)
            .await
            .unwrap();
        assert!(said.starts_with("added o/r to "), "{said}");
        assert!(said.contains("base trunk, gates: go vet ./...; go test ./..."), "{said}");
        assert!(said.contains("next: provefab doctor --json"), "{said}");
        let text = std::fs::read_to_string(paths.config()).unwrap();
        assert!(text.starts_with(MINE), "{text}");
        assert_eq!(
            &text[MINE.len()..],
            "\n\n[[repos]]\nslug = \"o/r\"\nlabel = \"provefab\"\nbase = \"trunk\"\ngates = [\"go vet ./...\", \"go test ./...\"]\n"
        );
        let config = Config::from_toml_str(&text).unwrap();
        let added = config.repos.last().unwrap();
        assert_eq!(
            (added.slug.as_str(), added.label.as_str(), added.base.as_str()),
            ("o/r", "provefab", "trunk")
        );
        assert!(added.local_path.is_none() && added.tracker.is_none() && added.risk.is_none());
        // A file ending with a newline gets one blank line before the block.
        let (_h2, paths2) = home_with(&format!("{MINE}\n"));
        repos_add(&paths2, &go_hub("main"), &git(), "o/r", None, false)
            .await
            .unwrap();
        let text2 = std::fs::read_to_string(paths2.config()).unwrap();
        assert!(text2.contains("# the end, no newline\n\n[[repos]]\nslug = \"o/r\""), "{text2}");
    }

    #[tokio::test]
    async fn a_branch_name_with_a_quote_is_escaped() {
        let (_h, paths) = home_with(MINE);
        repos_add(&paths, &go_hub("we\"ird"), &git(), "o/r", None, false)
            .await
            .unwrap();
        let config = Config::from_toml_str(&std::fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(config.repos.last().unwrap().base, "we\"ird");
    }

    /// Spec section 4: any case; plan decision 13: before GitHub is read.
    #[tokio::test]
    async fn a_repository_already_configured_in_any_case_is_refused_before_github_is_read() {
        let (_h, paths) = home_with(MINE);
        let hub = go_hub("main");
        let e = repos_add(&paths, &hub, &git(), "acme/API", None, false)
            .await
            .unwrap_err();
        assert_eq!(e.code(), 3, "{e}");
        assert!(e.to_string().contains("acme/API is already in"), "{e}");
        assert!(hub.repo_root_calls.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(paths.config()).unwrap(), MINE);
    }

    #[tokio::test]
    async fn dry_run_prints_the_block_and_writes_nothing() {
        let (_h, paths) = home_with(MINE);
        let shown = repos_add(&paths, &go_hub("main"), &git(), "o/r", None, true)
            .await
            .unwrap();
        assert_eq!(
            shown,
            "[[repos]]\nslug = \"o/r\"\nlabel = \"provefab\"\nbase = \"main\"\ngates = [\"go vet ./...\", \"go test ./...\"]\n"
        );
        assert_eq!(std::fs::read_to_string(paths.config()).unwrap(), MINE);
    }

    /// Spec section 4 and Review Focus 4: the whole file is validated before
    /// writing; the loader's multi-line error becomes one line.
    #[tokio::test]
    async fn a_file_that_would_not_load_is_refused_on_one_line() {
        let inline = "repos = []\n[jev]\nmodel = \"jev-1.13.0\"\n\n[[models]]\nid = \"c\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n";
        let (_h, paths) = home_with(inline);
        let e = repos_add(&paths, &go_hub("main"), &git(), "o/r", None, false)
            .await
            .unwrap_err();
        assert_eq!(e.code(), 1, "{e}");
        assert!(!e.to_string().contains('\n'), "{e}");
        assert!(e.to_string().contains("would not load with o/r"), "{e}");
        assert_eq!(std::fs::read_to_string(paths.config()).unwrap(), inline);
    }

    #[tokio::test]
    async fn usage_errors_exit_2() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let hub = go_hub("main");
        let e = repos_add(&paths, &hub, &git(), "o/r", None, false)
            .await
            .unwrap_err();
        assert_eq!(e.code(), 2);
        assert!(e.to_string().contains("run `provefab init` first"), "{e}");
        std::fs::write(paths.config(), MINE).unwrap();
        for bad in ["o", "o/r/x", "../r", "o/", "o r/x", "o/r\"]"] {
            let e = repos_add(&paths, &hub, &git(), bad, None, false)
                .await
                .unwrap_err();
            assert_eq!(e.code(), 2, "{bad}");
        }
        assert!(hub.repo_root_calls.lock().unwrap().is_empty());
        for good in ["o/r", "my-org/my.repo_2", "O/R"] {
            assert!(valid_slug(good), "{good}");
        }
    }

    /// Plan decision 18: Node and Python get the note about dependencies.
    #[tokio::test]
    async fn node_and_python_repositories_get_the_dependencies_note() {
        let (_h, paths) = home_with(MINE);
        let hub = FakeHub::new("x");
        *hub.repo_root.lock().unwrap() =
            Some(root_of(&[("package.json", r#"{"scripts":{"test":"jest"}}"#)], "main"));
        let said = repos_add(&paths, &hub, &git(), "o/web", None, false)
            .await
            .unwrap();
        assert!(said.contains(DEPENDENCIES_NOTE), "{said}");
        let (_h2, paths2) = home_with(MINE);
        let said = repos_add(&paths2, &go_hub("main"), &git(), "o/r", None, false)
            .await
            .unwrap();
        assert!(!said.contains(DEPENDENCIES_NOTE), "{said}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: FAIL to compile: `cannot find function repos_add`, `cannot find value DEPENDENCIES_NOTE`.

- [ ] **Step 3: Write the implementation**

After `node_gates` in `setup.rs`:

```rust
/// `owner/name` as GitHub allows it: letters, digits, `-`, `_`, `.`.
pub fn valid_slug(slug: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    matches!(slug.split('/').collect::<Vec<_>>().as_slice(), [o, n] if part(o) && part(n))
}

/// A TOML string, escaped (plan decision 12).
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// One `[[repos]]` block: slug, label, base, gates; nothing else (spec
/// section 4).
pub fn repo_block(slug: &str, base: &str, gates: &[String]) -> String {
    let gates: Vec<String> = gates.iter().map(|g| toml_string(g)).collect();
    format!(
        "[[repos]]\nslug = {}\nlabel = \"provefab\"\nbase = {}\ngates = [{}]\n",
        toml_string(slug),
        toml_string(base),
        gates.join(", ")
    )
}

pub const DEPENDENCIES_NOTE: &str = "note: a task's worktree starts without installed dependencies; if these commands need them, put the install command first in gates";

/// `provefab repos add` (spec section 4): the block on a dry run, else
/// what was added. The file is read, checked for the repository (any
/// case), extended in memory and validated whole, then appended to.
pub async fn repos_add(
    paths: &Paths,
    forge: &impl Forge,
    git: &Git,
    slug: &str,
    checkout: Option<&Path>,
    dry_run: bool,
) -> Result<String, SetupError> {
    if !valid_slug(slug) {
        return Err(SetupError::Usage(format!(
            "`{slug}` is not a GitHub repository as owner/name"
        )));
    }
    let file = paths.config();
    let existing = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SetupError::Usage(format!(
                "no configuration at {}: run `provefab init` first",
                file.display()
            )));
        }
        Err(e) => {
            return Err(SetupError::Other(format!(
                "cannot read {}: {e}",
                file.display()
            )));
        }
    };
    let config = Config::from_toml_str(&existing).map_err(|e| {
        SetupError::Other(format!(
            "{} does not load, fix it first: {}",
            file.display(),
            one_line(&e.to_string())
        ))
    })?;
    if config.repos.iter().any(|r| r.slug.eq_ignore_ascii_case(slug)) {
        return Err(SetupError::Exists(format!(
            "{slug} is already in {}; nothing changed",
            file.display()
        )));
    }
    let root = read_root(forge, git, slug, checkout).await?;
    let gates = detect(&root, slug)?;
    let block = repo_block(slug, &root.default_branch, &gates);
    let separator = if existing.is_empty() {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let added = format!("{separator}{block}");
    Config::from_toml_str(&format!("{existing}{added}")).map_err(|e| {
        SetupError::Other(format!(
            "{} would not load with {slug}, nothing written: {}",
            file.display(),
            one_line(&e.to_string())
        ))
    })?;
    if dry_run {
        return Ok(block);
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .and_then(|mut f| f.write_all(added.as_bytes()))
        .map_err(|e| SetupError::Other(format!("cannot write {}: {e}", file.display())))?;
    let note = match stacks(&root).as_slice() {
        [Stack::Node] | [Stack::Python] => format!("{DEPENDENCIES_NOTE}\n"),
        _ => String::new(),
    };
    Ok(format!(
        "added {slug} to {}: base {}, gates: {}\nProvefab keeps its own clone in {} (set local_path by hand to use yours)\n{note}next: provefab doctor --json\n",
        file.display(),
        root.default_branch,
        gates.join("; "),
        paths.home.join("repos").join(slug).display()
    ))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: PASS.

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/setup.rs
git commit -m "feat: provefab repos add appends a detected repository block

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Doctor's JSON lines and `fix` commands

**Files:**
- Modify: `crates/provefab/src/setup.rs` (after `repos_add`, and its tests)

**Interfaces:**
- Consumes: `commands::{Check, KEYCHAIN_SERVICE}`, `tracker::TrackerKind`, `rules::redact_credentials` (`pub(crate)`), `one_line` (Task 2).
- Produces:

```rust
pub const NO_CONFIG: &str = "no configuration at";
pub fn fix_for(check: &Check, config: Option<&Config>) -> Option<String>;
/// Today's rule: every check passed, the Jev key being optional (plan decision 5).
pub fn doctor_passed(checks: &[Check]) -> bool;
/// One JSON object per check and line: name, ok, detail (redacted), fix.
pub fn doctor_json(checks: &[Check], config: Option<&Config>) -> String;
/// The one line `doctor --json` prints when the configuration does not load.
pub fn config_check(paths: &Paths, e: &anyhow::Error) -> Check;
```

- [ ] **Step 1: Write the failing tests**

In `mod tests` of `setup.rs`, add:

```rust
    use crate::commands::Check;

    fn failed(name: &str, detail: &str) -> Check {
        Check {
            name: name.into(),
            ok: false,
            detail: detail.into(),
        }
    }

    const TRACKED: &str = "[jev]\nmodel = \"jev-1.13.0\"\n[[models]]\nid = \"m\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n[[repos]]\nslug = \"acme/api\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n[[repos]]\nslug = \"acme/web\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"linear\"\nproject = \"WEB\"\n";

    /// Spec section 5: `fix` for sign-in and key checks, only when failed.
    #[test]
    fn sign_in_and_key_checks_carry_the_command_that_fixes_them() {
        let config = Config::from_toml_str(TRACKED).unwrap();
        for (name, fix) in [
            ("gh login", Some("gh auth login")),
            ("claude login", Some("provefab login claude")),
            ("claude api key", Some("provefab login claude --api-key")),
            ("codex login", Some("provefab login codex")),
            ("codex guard hook", Some("provefab login codex")),
            ("codex api login", Some("provefab login codex --api-key")),
            ("codex api guard hook", Some("provefab login codex --api-key")),
            (
                "jev key",
                Some("security add-generic-password -s provefab-typesafe -a provefab -w"),
            ),
            (
                "tracker acme/api",
                Some("provefab login jira --site acme.atlassian.net"),
            ),
            ("tracker acme/web", Some("provefab login linear")),
            ("tracker other/repo", None),
            ("git", None),
            ("claude", None),
            ("repo acme/api", None),
            ("gates acme/api", None),
        ] {
            assert_eq!(
                fix_for(&failed(name, "x"), Some(&config)).as_deref(),
                fix,
                "{name}"
            );
            let passed = Check {
                ok: true,
                ..failed(name, "x")
            };
            assert_eq!(fix_for(&passed, Some(&config)), None, "{name}");
        }
        let missing = failed("configuration", &format!("{NO_CONFIG} /h/provefab.toml"));
        assert_eq!(fix_for(&missing, None).as_deref(), Some("provefab init"));
        let invalid = failed("configuration", "provefab.toml: repo `x` is not `owner/name`");
        assert_eq!(fix_for(&invalid, None), None);
    }

    /// Spec section 5 and Review Focus 5: the line shape, and no secret in
    /// any field.
    #[test]
    fn json_lines_have_the_shape_and_no_secret() {
        let checks = vec![
            Check {
                name: "git".into(),
                ok: true,
                detail: "git version 2.50".into(),
            },
            failed(
                "gh login",
                "token ghp_abcdefghijklmnopqrstuvwxyz0123456789 is invalid",
            ),
        ];
        let out = doctor_json(&checks, None);
        assert!(!out.contains("ghp_"), "{out}");
        let lines: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            serde_json::json!({"name": "git", "ok": true, "detail": "git version 2.50"})
        );
        assert_eq!(
            lines[1],
            serde_json::json!({
                "name": "gh login",
                "ok": false,
                "detail": "token <redacted> is invalid",
                "fix": "gh auth login",
            })
        );
    }

    /// Plan decision 5: the exit code ignores the optional Jev key only.
    #[test]
    fn doctor_passes_when_every_check_but_the_jev_key_passes() {
        let ok = Check {
            name: "git".into(),
            ok: true,
            detail: String::new(),
        };
        assert!(doctor_passed(&[ok.clone(), failed("jev key", "missing")]));
        assert!(!doctor_passed(&[ok.clone(), failed("gh login", "no")]));
        assert!(!doctor_passed(&[failed("merge settings", "refused")]));
        assert!(doctor_passed(&[]));
    }

    /// Plan decision 6: a missing file says so and points to `init`; a file
    /// that does not load is reported on one line.
    #[test]
    fn a_configuration_that_does_not_load_is_one_failed_line() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path());
        let missing = config_check(&paths, &anyhow::anyhow!("reading x: No such file"));
        assert_eq!((missing.name.as_str(), missing.ok), ("configuration", false));
        assert_eq!(
            missing.detail,
            format!("{NO_CONFIG} {}", paths.config().display())
        );
        std::fs::write(paths.config(), "[x").unwrap();
        let e = Config::from_toml_str("[x").unwrap_err();
        let bad = config_check(&paths, &anyhow::Error::from(e));
        assert!(!bad.ok && !bad.detail.contains('\n'), "{bad:?}");
        assert!(bad.detail.starts_with("provefab.toml: TOML parse error"), "{bad:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: FAIL to compile: `cannot find function fix_for`, `cannot find value NO_CONFIG`.

- [ ] **Step 3: Write the implementation**

Add `use crate::commands::Check;` to the imports of `setup.rs`, and after `repos_add`:

```rust
/// The start of the `configuration` line when there is no file.
pub const NO_CONFIG: &str = "no configuration at";

/// The command that fixes a failed check, when one does (spec section 5,
/// plan decision 7). Installing a missing tool is left to the person.
pub fn fix_for(check: &Check, config: Option<&Config>) -> Option<String> {
    if check.ok {
        return None;
    }
    let fix = match check.name.as_str() {
        "configuration" if check.detail.starts_with(NO_CONFIG) => "provefab init",
        "gh login" => "gh auth login",
        "claude login" => "provefab login claude",
        "claude api key" => "provefab login claude --api-key",
        "codex login" | "codex guard hook" => "provefab login codex",
        "codex api login" | "codex api guard hook" => "provefab login codex --api-key",
        "jev key" => {
            return Some(format!(
                "security add-generic-password -s {} -a provefab -w",
                crate::commands::KEYCHAIN_SERVICE
            ));
        }
        name => {
            let slug = name.strip_prefix("tracker ")?;
            let tracker = config?
                .repos
                .iter()
                .find(|r| r.slug == slug)?
                .tracker
                .as_ref()?;
            return match tracker.kind {
                crate::tracker::TrackerKind::Jira => Some(format!(
                    "provefab login jira --site {}",
                    tracker.site.as_deref()?
                )),
                crate::tracker::TrackerKind::Linear => Some("provefab login linear".into()),
                crate::tracker::TrackerKind::Github => None,
            };
        }
    };
    Some(fix.to_string())
}

/// Whether `doctor` succeeds: every check passed, the Jev key being
/// optional (plan decision 5, as `provefab doctor` always did).
pub fn doctor_passed(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok || c.name == "jev key")
}

/// `doctor --json` (spec section 5): one object per check and line, with
/// `fix` when a command fixes it. Details are redacted (plan decision 8).
pub fn doctor_json(checks: &[Check], config: Option<&Config>) -> String {
    let mut out = String::new();
    for c in checks {
        let mut line = serde_json::json!({
            "name": c.name,
            "ok": c.ok,
            "detail": crate::rules::redact_credentials(&c.detail),
        });
        if let Some(fix) = fix_for(c, config) {
            line["fix"] = Value::String(fix);
        }
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

/// The `configuration` line when the file is missing or does not load
/// (plan decision 6).
pub fn config_check(paths: &Paths, e: &anyhow::Error) -> Check {
    let detail = if paths.config().exists() {
        crate::rules::redact_credentials(&one_line(&e.to_string()))
    } else {
        format!("{NO_CONFIG} {}", paths.config().display())
    };
    Check {
        name: "configuration".into(),
        ok: false,
        detail,
    }
}
```

`e.to_string()` on an `anyhow::Error` built by `load_config` is the outermost message only (`reading <path>` for an I/O error, the `ConfigError` text for a parse error, which starts with `provefab.toml: `).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab setup::`
Expected: PASS.

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/setup.rs
git commit -m "feat: doctor checks as JSON lines with the command that fixes them

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The CLI: `init`, `repos add`, `doctor --json` and exit codes

**Files:**
- Modify: `crates/provefab/src/app.rs:9-23` (imports), `:36-112` (`enum Cmd`), after `enum ServiceAction` (`:114-127`), `dispatch` (`:420-480`, the `Cmd::Doctor` arm), new arms in `dispatch`
- Create: `crates/provefab/tests/setup.rs`

**Interfaces:**
- Consumes: `setup::{init, repos_add, doctor_json, doctor_passed, config_check, EXIT_CODES, SetupError}` (Tasks 2-5); `commands::{doctor, tracker_checks, rules_checks, Check, Tools}`.
- Produces: the commands `provefab init [--dry-run]`, `provefab repos add <owner/name> [--path <checkout>] [--dry-run]`, `provefab doctor [--json]`; exit codes 0 to 5 as in `EXIT_CODES`.

- [ ] **Step 1: Write the failing tests**

Create `crates/provefab/tests/setup.rs`:

```rust
//! Agent setup spec sections 3 to 6 through the binary: the commands an
//! agent runs and the exit codes it reads.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use provefab::testkit::git;

fn fake(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A directory used as the whole PATH, with the real `git` linked in.
fn bin_dir(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(out.stdout).unwrap().trim().to_string();
    std::os::unix::fs::symlink(real, bin.join("git")).unwrap();
    bin
}

fn provefab(home: &Path, path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_provefab"))
        .args(args)
        .env_clear()
        .env("PROVEFAB_HOME", home)
        .env("HOME", home)
        .env("PATH", path)
        .output()
        .unwrap()
}

/// Spec section 6: the exit code, and an error is one stderr line
/// starting with `provefab:`.
fn refused(out: &Output, code: i32) -> String {
    let err = String::from_utf8(out.stderr.clone()).unwrap();
    assert_eq!(out.status.code(), Some(code), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("provefab: "), "{err}");
    err
}

fn ok(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// A clone of a repository holding `files`, whose origin is
/// github.com/<slug> and whose `origin/HEAD` is `main`.
fn checkout(root: &Path, slug: &str, files: &[(&str, &str)]) -> PathBuf {
    let src = root.join(format!("src-{}", slug.replace('/', "-")));
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    for (name, text) in files {
        std::fs::write(src.join(name), text).unwrap();
    }
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "init"]);
    let dest = root.join(format!("clone-{}", slug.replace('/', "-")));
    git(root, &["clone", "-q", src.to_str().unwrap(), dest.to_str().unwrap()]);
    git(
        &dest,
        &["remote", "set-url", "origin", &format!("https://github.com/{slug}.git")],
    );
    dest
}

const CLAUDE: &str = r#"if [ "$1" = auth ]; then echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}'; exit 0; fi
echo '2.1.281 (Claude Code)'"#;

const GH_SIGNED_IN: &str = r#"case "$1" in
  auth) echo 'github.com'; exit 0 ;;
  api) echo 'error connecting to api.github.com' >&2; exit 1 ;;
  *) echo 'gh version 2.80' ;;
esac"#;

const GH_SIGNED_OUT: &str = r#"case "$1" in
  auth) echo 'You are not logged into any GitHub hosts. To log in, run: gh auth login' >&2; exit 1 ;;
  *) echo 'gh version 2.80' ;;
esac"#;

#[test]
fn an_agent_configures_a_repository_and_reads_each_exit_code() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let bin = bin_dir(t.path());
    fake(&bin, "gh", GH_SIGNED_IN);
    fake(&bin, "security", "exit 44");
    let config = home.join("provefab.toml");

    // No worker CLI on PATH: 4, nothing written.
    refused(&provefab(&home, &bin, &["init"]), 4);
    assert!(!config.exists());

    fake(&bin, "claude", CLAUDE);
    let said = ok(&provefab(&home, &bin, &["init"]));
    assert!(said.contains("models: claude-sonnet, claude-opus"), "{said}");
    assert!(config.exists());
    refused(&provefab(&home, &bin, &["init"]), 3);
    refused(&provefab(&home, &bin, &["init", "--dry-run"]), 3);

    // Usage errors: clap's, then ours.
    assert_eq!(provefab(&home, &bin, &["repos", "add"]).status.code(), Some(2));
    refused(&provefab(&home, &bin, &["repos", "add", "not-a-slug"]), 2);
    let rust = checkout(t.path(), "o/r", &[("Cargo.toml", "[package]\nname = \"r\"\n")]);
    let elsewhere = checkout(t.path(), "o/other", &[("go.mod", "module x\n")]);
    refused(
        &provefab(&home, &bin, &["repos", "add", "o/r", "--path", elsewhere.to_str().unwrap()]),
        2,
    );

    // Through gh: gh cannot reach GitHub here, so 5.
    refused(&provefab(&home, &bin, &["repos", "add", "o/r"]), 5);

    let before = std::fs::read_to_string(&config).unwrap();
    let shown = ok(&provefab(
        &home,
        &bin,
        &["repos", "add", "o/r", "--path", rust.to_str().unwrap(), "--dry-run"],
    ));
    assert!(shown.starts_with("[[repos]]\nslug = \"o/r\""), "{shown}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    ok(&provefab(&home, &bin, &["repos", "add", "o/r", "--path", rust.to_str().unwrap()]));
    let after = std::fs::read_to_string(&config).unwrap();
    assert!(after.starts_with(&before), "{after}");
    assert!(after.contains("gates = [\"cargo fmt -- --check\""), "{after}");
    refused(
        &provefab(&home, &bin, &["repos", "add", "O/R", "--path", rust.to_str().unwrap()]),
        3,
    );

    // Not recognised: 4.
    let docs = checkout(t.path(), "o/docs", &[("README.md", "# docs\n")]);
    refused(
        &provefab(&home, &bin, &["repos", "add", "o/docs", "--path", docs.to_str().unwrap()]),
        4,
    );

    // doctor --json: every line is an object with name, ok and detail; all
    // pass but the optional Jev key, so 0.
    let out = provefab(&home, &bin, &["doctor", "--json"]);
    let lines = ok(&out);
    let parsed: Vec<serde_json::Value> = lines
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(!parsed.is_empty());
    for l in &parsed {
        assert!(l["name"].is_string() && l["ok"].is_boolean() && l["detail"].is_string(), "{l}");
    }
    let jev = parsed.iter().find(|l| l["name"] == "jev key").unwrap();
    assert_eq!(
        (jev["ok"].as_bool(), jev["fix"].as_str()),
        (
            Some(false),
            Some("security add-generic-password -s provefab-typesafe -a provefab -w")
        )
    );

    // A failed sign-in: 1, with its fix.
    fake(&bin, "gh", GH_SIGNED_OUT);
    let out = provefab(&home, &bin, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    let gh = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|l| l["name"] == "gh login")
        .unwrap();
    assert_eq!((gh["ok"].as_bool(), gh["fix"].as_str()), (Some(false), Some("gh auth login")));
    // Without --json the output is the text one, and the code the same.
    let text = provefab(&home, &bin, &["doctor"]);
    assert_eq!(text.status.code(), Some(1));
    assert!(String::from_utf8(text.stdout).unwrap().contains("FAIL gh login"));
}

/// Plan decision 6: no configuration yet is one `configuration` line, exit 1.
#[test]
fn doctor_json_without_a_configuration_points_to_init() {
    let t = tempfile::tempdir().unwrap();
    let bin = bin_dir(t.path());
    let out = provefab(&t.path().join("home"), &bin, &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let line: serde_json::Value =
        serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim()).unwrap();
    assert_eq!(
        (line["name"].as_str(), line["ok"].as_bool(), line["fix"].as_str()),
        (Some("configuration"), Some(false), Some("provefab init"))
    );
}

/// Spec section 6: the codes are in `--help` of the new commands.
#[test]
fn the_new_commands_document_their_exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let bin = bin_dir(t.path());
    for args in [
        vec!["init", "--help"],
        vec!["repos", "add", "--help"],
        vec!["doctor", "--help"],
    ] {
        let help = ok(&provefab(t.path(), &bin, &args));
        // Whitespace-insensitive: clap may wrap the text.
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            help.contains("3 the configuration or the repository already exists"),
            "{args:?}: {help}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --all-features -p provefab --test setup`
Expected: FAIL: `init` is not a subcommand (clap prints `error: unrecognized subcommand 'init'`, exit 2, so `an_agent_configures_a_repository_and_reads_each_exit_code` fails on its first `refused(.., 4)`), and `--json` is not an argument of `doctor`.

- [ ] **Step 3: Write the implementation**

`crates/provefab/src/app.rs` imports: add `use crate::commands::Check;` next to `use crate::commands::{self, Tools};` (or extend it to `use crate::commands::{self, Check, Tools};`) and `use crate::setup::{self, SetupError};`.

In `enum Cmd`, replace the `Doctor` variant and add two variants after it:

```rust
    /// Check tools, logins, the Jev key and the repos.
    #[command(after_help = setup::EXIT_CODES)]
    Doctor {
        /// One JSON object per check and line: name, ok, detail, and fix
        /// when a command fixes it.
        #[arg(long)]
        json: bool,
    },
    /// Write ~/.provefab/provefab.toml, with the models of the worker CLIs on PATH.
    #[command(after_help = setup::EXIT_CODES)]
    Init {
        /// Print the file instead of writing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// The repositories Provefab watches.
    #[command(after_help = setup::EXIT_CODES)]
    Repos {
        #[command(subcommand)]
        action: ReposAction,
    },
```

After `enum ServiceAction`:

```rust
#[derive(Subcommand)]
enum ReposAction {
    /// Append one [[repos]] block to provefab.toml: base from the default
    /// branch, gates detected from the files at the repository's root.
    #[command(after_help = setup::EXIT_CODES)]
    Add {
        /// The GitHub repository, as owner/name.
        slug: String,
        /// A local clone of that repository, read instead of GitHub.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print the block instead of adding it.
        #[arg(long)]
        dry_run: bool,
    },
}

/// A setup command's output, or its error on one stderr line with its exit
/// code (spec section 6).
fn setup_exit(r: Result<String, SetupError>) -> ExitCode {
    match r {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("provefab: {e}");
            ExitCode::from(e.code())
        }
    }
}
```

In `dispatch`, replace the `Cmd::Doctor => { ... }` arm with:

```rust
        Cmd::Doctor { json } => {
            let config = match load_config(&paths) {
                Ok(c) => c,
                Err(e) if json => {
                    print!(
                        "{}",
                        setup::doctor_json(&[setup::config_check(&paths, &e)], None)
                    );
                    return Ok(ExitCode::FAILURE);
                }
                Err(e) => return Err(e),
            };
            let policy = ext.policy.clone();
            let warnings = policy.warnings(&config);
            let refused = policy.check(&config).err().map(|e| Check {
                name: "merge settings".into(),
                ok: false,
                detail: e.to_string(),
            });
            if !json {
                for w in &warnings {
                    println!("warn {w}");
                }
                if let Some(c) = &refused {
                    println!("FAIL {:<22} {}", c.name, c.detail);
                }
            }
            let oracle = oracle(&config).await?;
            let mut checks = commands::doctor(
                &Tools::default(),
                &config,
                &paths,
                oracle.as_ref().map(|o| &o.client),
            )
            .await;
            checks.extend(
                commands::tracker_checks(&Tools::default(), &config, &crate::tracker::process_env)
                    .await,
            );
            checks.extend(commands::rules_checks(&Tools::default(), &config, &paths, &gh()).await);
            if json {
                // Plan decision 6: every finding is a line.
                let mut all: Vec<Check> = warnings
                    .into_iter()
                    .map(|w| Check {
                        name: "warning".into(),
                        ok: true,
                        detail: w,
                    })
                    .collect();
                all.extend(refused);
                all.extend(checks);
                print!("{}", setup::doctor_json(&all, Some(&config)));
                return Ok(if setup::doctor_passed(&all) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
            for c in &checks {
                println!(
                    "{} {:<22} {}",
                    if c.ok { "ok  " } else { "FAIL" },
                    c.name,
                    c.detail
                );
            }
            Ok(if refused.is_none() && setup::doctor_passed(&checks) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Init { dry_run } => Ok(setup_exit(setup::init(
            &paths,
            &std::env::var_os("PATH").unwrap_or_default(),
            dry_run,
        ))),
        Cmd::Repos {
            action:
                ReposAction::Add {
                    slug,
                    path,
                    dry_run,
                },
        } => {
            let git = Git {
                program: "git".into(),
            };
            Ok(setup_exit(
                setup::repos_add(&paths, &gh(), &git, &slug, path.as_deref(), dry_run).await,
            ))
        }
```

The text branch prints the same lines in the same order as before (warnings, the refused merge setting, then every check) and keeps the same exit rule (`doctor_passed` is the old `ok &= c.ok || c.name == "jev key"`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --all-features -p provefab --test setup --test app`
Expected: PASS.

- [ ] **Step 5: Run the checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/src/app.rs crates/provefab/tests/setup.rs
git commit -m "feat: provefab init, repos add and doctor --json with exit codes

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Agent guide, documentation, spec amendments, version 0.6.0, real run

**Files:**
- Create: `docs/guide/agents.md`
- Modify: `README.md` (quick start, documentation list, day to day table), `docs/guide/configuration.md` (intro, `[[repos]]` section, "Changing the configuration"), `docs/guide/operations.md` (troubleshooting intro, new "Exit codes" section), `docs/specs/2026-10-02-agent-setup-design.md`, `crates/provefab/Cargo.toml:3`, `Cargo.lock`

**Interfaces:**
- Consumes: the behaviour as built in Tasks 1-6 (read `setup.rs` and `app.rs` before writing; where this text and the code disagree, the code wins and the text is fixed).
- Produces: `docs/guide/agents.md`, which the landing plan publishes at `/docs/agents/` (its first paragraph becomes the page's meta description: keep it a paragraph that does not end with a colon).

- [ ] **Step 1: Write `docs/guide/agents.md`**

````markdown
# Set up Provefab with a coding agent

This page is for a coding agent, such as Claude Code or Codex, that a person asked to set up Provefab on a repository. The agent writes and checks Provefab's configuration with commands that need no editor. The person keeps the installation, the sign-ins, the keys and the service, and runs those commands themselves.

## Rules

- Never handle a secret: do not ask for, type, read or store a token, an API key or a password.
- Never install Provefab or its service, never put a label on an issue, never run `provefab run`.
- Run only the commands on this page. When one fails, report its error instead of working around it.

## Steps

1. **Check that Provefab is installed:** `provefab --version`. If the command is not found, ask the person to run this line in their own terminal, and stop:

   ```bash
   curl -fsSL https://provefab.com/install.sh | sh
   ```

2. **Write the configuration:** `provefab init`.
   - Exit code 0: `~/.provefab/provefab.toml` is written, with the models of the worker CLIs found on `PATH`.
   - Exit code 3: a configuration already exists. Keep it and go on.
   - Exit code 4: no worker CLI was found. Ask the person to install Claude Code (`claude`) or the Codex CLI (`codex`), and stop.

3. **Add the repository the person is in.** From its root, with `<owner/name>` the GitHub repository its `origin` remote points to (`git remote get-url origin`):

   ```bash
   provefab repos add <owner/name> --path .
   ```

   - Exit code 0: one `[[repos]]` block was added, with the default branch as `base` and the checks detected from the files at the repository's root as `gates`.
   - Exit code 3: the repository is already configured. Go on.
   - Exit code 4: the repository was not recognised (no Rust, Node, Python or Go project at its root, or several). Report the error and point the person to the [configuration guide](configuration.md): its block is written by hand.
   - Exit code 2: check the arguments (`<owner/name>`, and that the directory is a clone of that repository).
   - Exit code 5: `gh` or the network failed. Report the error.

4. **Check everything:** `provefab doctor --json`. It prints one JSON object per line: `name`, `ok`, `detail`, and `fix` when a command fixes the problem.
   - For each line with `"ok": false` and a `fix`, show the person the `fix` command to run in their own terminal. Do not run it yourself: these commands sign in or store a key.
   - For a failed line without a `fix`, show its `detail`.
   - When the person says they are done, run `provefab doctor --json` again.
   - `jev key` is optional: without a TypeSafe key, Provefab runs with cautious defaults. The exit code is 0 when every other check passed.

5. **End with a short summary for the person:**
   - what is configured: the file's path, the models, the repository with its `base` and `gates`;
   - the commands left for them: the `fix` commands that still fail, `provefab run --dry-run` to preview, `provefab service install --workers 1` to start the service, and the `provefab` label on an issue when they want Provefab to work on it.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a doctor check failed, or another error (the message says which) |
| 2 | usage error, or no configuration yet (run `provefab init`) |
| 3 | the configuration or the repository already exists |
| 4 | stack or worker not recognised |
| 5 | `gh` or network error |

Errors are one line on stderr starting with `provefab:`.
````

- [ ] **Step 2: Update the other docs**

`README.md`:
- After the "Requirements: ..." line of "Quick start", add: `With a coding agent, point it at [Set up Provefab with a coding agent](docs/guide/agents.md): it writes and checks the configuration, and leaves the sign-ins, keys and service to you.`
- Step 1 of the code block becomes:

```bash
# 1. Install the binary (it embeds the worker plugins), one of:
#    a. the installer (macOS, into ~/.local/bin, no sudo):
curl -fsSL https://provefab.com/install.sh | sh
#    b. the signed and notarized build, no Rust needed: download
#       provefab-<version>-macos-universal.zip from the Releases page, unzip, then
sudo install -m 755 provefab /usr/local/bin/provefab
#    c. from source, with Rust 1.96 or newer:
cargo install --git https://github.com/provefab/provefab --tag v0.6.0 --locked provefab
```

- Step 4 becomes:

```bash
# 4. Configure, then check. `init` writes ~/.provefab/provefab.toml with the
#    models of the workers it finds; `repos add` detects your checks
#    (add --path <your clone> to read it instead of GitHub).
provefab init
provefab repos add your-account/your-repo
provefab doctor
```

- Day to day table, after the `provefab doctor` row: `| add a repository, with its checks detected | \`provefab repos add <owner/name>\` |`
- Documentation list, first item: `- [Set up with a coding agent](docs/guide/agents.md): what an agent runs to configure Provefab, and what it leaves to you.`

`docs/guide/configuration.md`:
- After the intro paragraph ending "Run it after every change.", add:

```markdown
## Writing the file with commands

`provefab init` writes `provefab.toml` when there is none: the `[jev]` section and a catalog with the models of the worker CLIs on your `PATH` (`claude` gives `claude-sonnet` and `claude-opus`, `codex` gives `codex-gpt`, as in the example). It adds no repository, and never changes an existing file. `--dry-run` prints the file instead.

`provefab repos add <owner/name>` appends one `[[repos]]` block at the end of the file and keeps the rest as you wrote it: `slug`, `label = "provefab"`, `base` (the repository's default branch) and `gates`, detected from the files at the root of the default branch:

| At the root | `gates` |
|---|---|
| `Cargo.toml` | `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |
| `package.json` | `<pm> run lint`, `<pm> run typecheck`, `<pm> test`, for the scripts that exist; `<pm>` is `pnpm` with `pnpm-lock.yaml`, `yarn` with `yarn.lock`, `npm` otherwise |
| `pyproject.toml`, or `setup.py` or `setup.cfg` with a `tests` directory | `ruff check .` when ruff is configured (`[tool.ruff]` or `ruff.toml`), then `pytest` |
| `go.mod` | `go vet ./...`, `go test ./...` |

It reads these files through `gh` without cloning, or from your clone with `--path <checkout>` (its `origin` must be that GitHub repository). A repository with none of these, with several, or with a `package.json` without any of the three scripts is refused: write its block by hand. `repos add` never writes `local_path`, a tracker, a risk policy or post-merge checks: add them by hand. A task's worktree starts without installed dependencies: if your checks need them, put the install command first in `gates`. `--dry-run` prints the block instead. The commands' exit codes are in [Operations](operations.md#exit-codes).
```

- In "Changing the configuration", the "New repository" bullet becomes: `- **New repository:** \`provefab repos add <owner/name>\`, or add a \`[[repos]]\` block by hand; then run \`provefab doctor\`, then \`provefab run --dry-run\` to see how its issues would be handled.`

`docs/guide/operations.md`:
- The first line of "Troubleshooting" becomes: `Always start with \`provefab doctor\`. Each \`FAIL\` line says what to do. \`provefab doctor --json\` prints the same checks as one JSON object per line (\`name\`, \`ok\`, \`detail\`), with \`fix\`, the command to run, for sign-ins and keys.`
- Before "## Troubleshooting", add:

```markdown
## Exit codes

`provefab init`, `provefab repos add` and `provefab doctor` exit with:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a `doctor` check failed (the Jev key is optional and never fails it), or another error |
| 2 | usage error, or no configuration yet (run `provefab init`) |
| 3 | the configuration or the repository already exists |
| 4 | stack or worker not recognised |
| 5 | `gh` or network error |

Errors are one line on stderr starting with `provefab:`.
```

- [ ] **Step 3: Spec amendments**

In `docs/specs/2026-10-02-agent-setup-design.md`, each marked "(amended 2026-10-02 in the plan)": §4 `--path` reads the clone's committed default branch, must have `origin` at `github.com/<owner/name>` (exit 2 otherwise) and is not written as `local_path` (plan decisions 2, 3); a stack counts by its marker and npm's placeholder test script is no gate (decisions 9, 10); the duplicate check runs before GitHub is read (decision 13); §5 the Jev key stays optional for the exit code, warnings, refused merge settings and a missing configuration are lines, details are redacted (decisions 5, 6, 8); §6 exit 1 also covers a file that does not load and I/O errors, a missing configuration for `repos add` is 2, `init --dry-run` on an existing file is 3 (decision 4); §11 one new `Forge` method (`repo_root`) and three `Git` methods, no new module (decision 1); §12 add decision 6: Node and Python dependencies are the owner's, noted at `repos add` (decision 18).

- [ ] **Step 4: Version 0.6.0**

Set `version = "0.6.0"` in `crates/provefab/Cargo.toml`, then `cargo build -p provefab` (updates `Cargo.lock`).

- [ ] **Step 5: Checks and the copy rules**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green (`no_paid_code` still clean).

Run: `grep -n "—" docs/guide/*.md README.md crates/provefab/src/setup.rs crates/provefab/src/app.rs`
Expected: no output.

Run: `grep -rniE "guarantee|automatically configur|fully automatic" docs/guide/agents.md docs/guide/configuration.md`
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add docs README.md crates/provefab/Cargo.toml Cargo.lock
git commit -m "docs: setup with a coding agent; provefab 0.6.0

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Real run before release (owner, not the implementer)**

Spec §10: in an isolated home (`export PROVEFAB_HOME=$(mktemp -d)`), from a fresh clone of the sandbox repository, open Claude Code and say only: `Set up Provefab on this repository: follow docs/guide/agents.md in the provefab repository` (the site page once the landing release is out). Evidence to keep: the transcript (it runs `provefab init`, `provefab repos add <owner/name> --path .`, `provefab doctor --json`, shows each `fix` without running it, and ends with the summary), the written `provefab.toml` (the `[[repos]]` block appended after `init`'s text), and no secret, label, `provefab run` or `service install` in the transcript.

---

## Self-review

**Spec coverage.** §1-2 owner decisions: configuration only, gates from detection only (Tasks 3, 4; the agent's limits in Task 7's guide). §3 `init`: home from `PROVEFAB_HOME` (`Paths::from_env` in Task 6), refuse with 3, catalog from `PATH`, same entries as the example, exit 4 without a CLI, no repository, `--dry-run`, loader check (Task 2). §4 `repos add`: requires the file, appends without rewriting, duplicate any case (Task 4); `--path` or `gh` without cloning (Tasks 1, 3); base is the default branch (Tasks 1, 3); detection per stack and its errors (Task 3); the block's four keys, whole file validated, `--dry-run` (Task 4). §5 `doctor --json`: line shape, `fix`, unchanged text, exit codes, no secret (Tasks 5, 6). §6 exit codes, one stderr line, `--help` (Tasks 2, 6) and agent guide (Task 7). §7 the agent guide (Task 7; its `llms.txt` link is in the landing plan). §8 in the landing plan. §9 docs (Task 7). §10 tests: detection both ways (Task 3), `init` (Task 2), `repos add` (Task 4), `doctor --json` (Tasks 5, 6), exit codes through the binary (Task 6), real run (Task 7 Step 7). §11 budget: one module, no migration, no dependency, 0.6.0 (Task 7); the port method is decision 1.

**Placeholder scan.** No "TBD", "TODO", "similar to" or unshown code; every code step carries its code, every run step its command and expected outcome.

**Type consistency.** `RepoRoot { default_branch, entries: BTreeSet<String>, files: BTreeMap<String, String> }` in Tasks 1, 3, 4; `Forge::repo_root(slug, read: &[&str])` in Tasks 1, 3; `Git::{origin_url -> Option<String>, origin_head -> Option<String>, root_entries -> Result<BTreeSet<String>, _>}` in Tasks 1, 3; `FakeHub::{repo_root, repo_root_calls}` in Tasks 1, 3, 4; `SetupError::{Usage, Exists, NotRecognised, Gh, Other}` and `code() -> u8` in Tasks 2-6; `init(paths, path_var, dry_run)`, `repos_add(paths, forge, git, slug, checkout, dry_run)` in Tasks 2, 4, 6; `fix_for(check, Option<&Config>)`, `doctor_json(checks, Option<&Config>)`, `doctor_passed`, `config_check(paths, &anyhow::Error)` in Tasks 5, 6; `EXIT_CODES` in Tasks 2, 6.

**Review Focus.** Each line has its test in the owning task: 1 in Task 3, 2 in Task 4, 3 in Task 3 (the `npm init` case), 4 in Tasks 4 and 6, 5 in Tasks 5 and 3.
