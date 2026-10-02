//! What an agent runs to configure Provefab (agent setup spec): `init`,
//! `repos add`, the JSON lines of `doctor --json`, and their exit codes.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::forge::{ForgeError, Git, RepoRoot};
use crate::paths::Paths;
use crate::ports::Forge;

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
    // Parsed before anything is written, so a failure leaves no file.
    let ids: Vec<String> = Config::from_toml_str(&text)
        .map_err(|e| SetupError::Other(one_line(&e.to_string())))?
        .models
        .into_iter()
        .map(|m| m.id)
        .collect();
    std::fs::create_dir_all(&paths.home)
        .map_err(|e| SetupError::Other(format!("cannot create {}: {e}", paths.home.display())))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => exists(),
            _ => SetupError::Other(format!("cannot write {}: {e}", file.display())),
        })?;
    if let Err(e) = f.write_all(text.as_bytes()) {
        // A partial file would make the next `init` exit 3 (best effort).
        drop(f);
        let _ = std::fs::remove_file(&file);
        return Err(SetupError::Other(format!(
            "cannot write {}: {e}",
            file.display()
        )));
    }
    Ok(format!(
        "wrote {}\nmodels: {}\nnext: provefab repos add <owner/name>\n",
        file.display(),
        ids.join(", ")
    ))
}

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
        _ => "GitHub did not answer through gh; check `gh auth status` and the network".to_string(),
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
                    .is_some_and(|t| t.get("tool").and_then(|tool| tool.get("ruff")).is_some());
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
/// manager of the lock file (spec section 4, plan decision 10). Controller
/// ruling over the brief: the gates start with that lock file's install
/// command (`npm ci` when `package-lock.json` is at the root, else
/// `npm install`), so a fresh worktree has its dependencies.
fn node_gates(root: &RepoRoot, slug: &str) -> Result<Vec<String>, SetupError> {
    let has = |n: &str| root.entries.contains(n);
    let (pm, install) = if has("pnpm-lock.yaml") {
        ("pnpm", "pnpm install --frozen-lockfile")
    } else if has("yarn.lock") {
        ("yarn", "yarn install --frozen-lockfile")
    } else if has("package-lock.json") {
        ("npm", "npm ci")
    } else {
        ("npm", "npm install")
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
    gates.insert(0, install.to_string());
    Ok(gates)
}

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
                config
                    .models
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>(),
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
        assert!(
            said.contains("models: claude-sonnet, claude-opus"),
            "{said}"
        );
        assert!(
            said.contains("next: provefab repos add <owner/name>"),
            "{said}"
        );
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
            &[
                "clone",
                "-q",
                src.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
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

    const NPM_INIT: &str =
        r#"{"name":"x","scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#;

    /// Spec section 10: each stack from fixture files, read from a clone
    /// (`--path`) and through GitHub (FakeHub serving the same files).
    #[tokio::test]
    async fn detection_per_stack_from_a_clone_and_through_github() {
        type Want = Result<Vec<&'static str>, &'static str>;
        type Case = (&'static str, Vec<(&'static str, &'static str)>, Want);
        let cases: Vec<Case> = vec![
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
                Ok(vec![
                    "pnpm install --frozen-lockfile",
                    "pnpm run lint",
                    "pnpm run typecheck",
                    "pnpm test",
                ]),
            ),
            (
                "yarn",
                vec![
                    ("package.json", r#"{"scripts":{"test":"jest"}}"#),
                    ("yarn.lock", ""),
                ],
                Ok(vec!["yarn install --frozen-lockfile", "yarn test"]),
            ),
            (
                "npm with scripts",
                vec![(
                    "package.json",
                    r#"{"scripts":{"lint":"eslint .","test":"node --test"}}"#,
                )],
                Ok(vec!["npm install", "npm run lint", "npm test"]),
            ),
            (
                "npm with package-lock",
                vec![
                    ("package.json", r#"{"scripts":{"test":"node --test"}}"#),
                    ("package-lock.json", "{}"),
                ],
                Ok(vec!["npm ci", "npm test"]),
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
        assert!(
            !said.contains("ghp_") && !said.contains("other/repo"),
            "{said}"
        );
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
            (
                5,
                "o/r was not found on GitHub, or gh's account cannot read it"
            )
        );
        let spawn = ForgeError::Spawn {
            program: "gh".into(),
            args: "api".into(),
            message: "No such file or directory".into(),
        };
        assert_eq!(
            gh_error("o/r", &spawn).to_string(),
            "gh is not installed or cannot run"
        );
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
}
