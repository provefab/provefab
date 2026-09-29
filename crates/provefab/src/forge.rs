//! Git and GitHub, run by Provefab itself (never inside a worker): task
//! worktrees and branches, Provefab's commit and push, and `gh` for issues,
//! comments, labels and pull requests (spec §3.1, §3.2).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// First line of every comment Provefab posts (spec §3.4 item 4, D33).
pub const BOT_PREFIX: &str = "*Posted by Provefab (automated), not typed by a person.*";
/// The prefix used before the rename (plan 5). Comments already on GitHub keep
/// it, so it still marks the bot's own comments.
pub const LEGACY_BOT_PREFIX: &str =
    "*Posted by the software factory (automated), not typed by a person.*";

/// Whether a comment was posted by Provefab, under its current or former name.
pub fn is_bot_comment(body: &str) -> bool {
    body.starts_with(BOT_PREFIX) || body.starts_with(LEGACY_BOT_PREFIX)
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("{program} {args}: could not start: {message}")]
    Spawn {
        program: String,
        args: String,
        message: String,
    },
    #[error("{program} {args}: exit {code:?}: {stderr}")]
    Failed {
        program: String,
        args: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("unreadable output from {0}: {1}")]
    Parse(String, String),
    #[error("{program} {args}: no answer after {secs}s")]
    Timeout {
        program: String,
        args: String,
        secs: u64,
    },
    #[error("{path} is a worktree on branch `{actual}`, not `{wanted}`")]
    WrongWorktree {
        path: String,
        actual: String,
        wanted: String,
    },
}

impl ForgeError {
    /// True when waiting cannot help: a worktree on the wrong branch, or an
    /// issue or repository that does not exist or is not accessible.
    pub fn is_permanent(&self) -> bool {
        match self {
            ForgeError::WrongWorktree { .. } => true,
            ForgeError::Failed { stderr, .. } => {
                let stderr = stderr.to_lowercase();
                stderr.contains("could not resolve to an issue")
                    || stderr.contains("could not resolve to a repository")
                    || stderr.contains("repository not found")
            }
            ForgeError::Spawn { .. } | ForgeError::Parse(..) | ForgeError::Timeout { .. } => false,
        }
    }
}

/// Budget for one git or gh call (a push or an API call over a slow link).
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

async fn run(
    program: &Path,
    cwd: Option<&Path>,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<String, ForgeError> {
    run_with_timeout(program, cwd, args, stdin, RUN_TIMEOUT).await
}

/// Runs a git or gh command. Never waits on a terminal prompt (credentials,
/// passphrases) and gives up after `timeout`; the child is killed on drop.
async fn run_with_timeout(
    program: &Path,
    cwd: Option<&Path>,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, ForgeError> {
    let shown = args.join(" ");
    let name = program.display().to_string();
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        // Provefab's own git takes its config from files only: a
        // `GIT_CONFIG_*` inherited from a parent (the workers' no-push setup,
        // when Provefab's tests run inside its own gates) must not stop the
        // one component that is meant to push.
        .env_remove("GIT_CONFIG_COUNT")
        .env("GH_PROMPT_DISABLED", "1")
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(|e| ForgeError::Spawn {
        program: name.clone(),
        args: shown.clone(),
        message: e.to_string(),
    })?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes()).await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| ForgeError::Timeout {
            program: name.clone(),
            args: shown.clone(),
            secs: timeout.as_secs(),
        })?
        .map_err(|e| ForgeError::Spawn {
            program: name.clone(),
            args: shown.clone(),
            message: e.to_string(),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(2000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(ForgeError::Failed {
            program: name,
            args: shown,
            code: out.status.code(),
            stderr: tail.trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// `provefab/<issue>-<slug of the title>`, at most 60 characters.
pub fn branch_name(issue: u64, title: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let prefix = format!("provefab/{issue}-");
    let room = 60usize.saturating_sub(prefix.len());
    let slug: String = slug.chars().take(room).collect();
    format!("{prefix}{}", slug.trim_end_matches('-'))
}

pub struct Git {
    pub program: PathBuf,
}

impl Git {
    /// Every git call Provefab makes runs with Provefab's credentials, so
    /// it never runs repository hooks: a gate (husky's `prepare`, say) can point
    /// `core.hooksPath` at files the agent writes (review C1).
    async fn git(&self, cwd: &Path, args: &[&str]) -> Result<String, ForgeError> {
        let mut all = vec!["-c", "core.hooksPath=/dev/null"];
        all.extend_from_slice(args);
        run(&self.program, Some(cwd), &all, None).await
    }

    /// Creates the task worktree on a new branch from `base`, or reattaches an
    /// existing branch (a resumed task).
    pub async fn worktree_add(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<(), ForgeError> {
        let dir = path.display().to_string();
        // Forget worktrees whose directory was deleted by hand, so the branch can be reattached.
        self.git(repo, &["worktree", "prune"]).await?;
        if path.join(".git").exists() {
            let actual = self
                .git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .await?;
            if actual != branch {
                return Err(ForgeError::WrongWorktree {
                    path: dir,
                    actual,
                    wanted: branch.to_string(),
                });
            }
            return self.exclude_provefab_dir(path).await;
        }
        let exists = self
            .git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .await
            .is_ok();
        if exists {
            self.git(repo, &["worktree", "add", &dir, branch]).await?;
        } else {
            // `--no-track` (and a push without `-u`): parallel tasks share
            // `.git/config`, and a tracking entry makes them race for its lock.
            self.git(
                repo,
                &["worktree", "add", "--no-track", "-b", branch, &dir, base],
            )
            .await?;
        }
        self.exclude_provefab_dir(path).await
    }

    /// Adds `.provefab/` (stage outputs mirrored into the worktree) to the
    /// repository's `info/exclude`, so neither `git status` nor a commit sees it.
    async fn exclude_provefab_dir(&self, worktree: &Path) -> Result<(), ForgeError> {
        let common = self
            .git(
                worktree,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
        let exclude = PathBuf::from(common).join("info").join("exclude");
        let current = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !current.lines().any(|l| l.trim() == ".provefab/") {
            let io =
                |e: std::io::Error| ForgeError::Parse(exclude.display().to_string(), e.to_string());
            if let Some(dir) = exclude.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            let sep = if current.is_empty() || current.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            std::fs::write(&exclude, format!("{current}{sep}.provefab/\n")).map_err(io)?;
        }
        Ok(())
    }

    pub async fn worktree_remove(&self, repo: &Path, path: &Path) -> Result<(), ForgeError> {
        let dir = path.display().to_string();
        self.git(repo, &["worktree", "remove", "--force", &dir])
            .await?;
        Ok(())
    }

    /// A fresh detached worktree at `commit`. Anything already at `path` (a
    /// crashed run's residue) is discarded first, never reused (spec section 9).
    pub async fn worktree_fresh_detached(
        &self,
        repo: &Path,
        path: &Path,
        commit: &str,
    ) -> Result<(), ForgeError> {
        self.worktree_discard(repo, path).await?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ForgeError::Parse(parent.display().to_string(), e.to_string()))?;
        }
        let dir = path.display().to_string();
        self.git(repo, &["worktree", "add", "--detach", &dir, commit])
            .await?;
        self.exclude_provefab_dir(path).await
    }

    /// Removes a worktree and its directory, registered or not. No-op when absent.
    pub async fn worktree_discard(&self, repo: &Path, path: &Path) -> Result<(), ForgeError> {
        if path.exists() {
            let dir = path.display().to_string();
            // Not a registered worktree (plain residue): git refuses; the
            // directory is removed below either way.
            let _ = self
                .git(repo, &["worktree", "remove", "--force", &dir])
                .await;
            if path.exists() {
                std::fs::remove_dir_all(path)
                    .map_err(|e| ForgeError::Parse(dir.clone(), e.to_string()))?;
            }
        }
        self.git(repo, &["worktree", "prune"]).await?;
        Ok(())
    }

    /// 1 for an ordinary or squash commit, 2 for a merge commit.
    pub async fn parent_count(&self, repo: &Path, commit: &str) -> Result<usize, ForgeError> {
        let line = self
            .git(repo, &["rev-list", "--parents", "-n", "1", commit])
            .await?;
        Ok(line.split_whitespace().count().saturating_sub(1))
    }

    /// Reverts one commit with hooks off; `mainline` is `Some(1)` for a merge
    /// commit. A conflict is returned as the git error, never resolved.
    pub async fn revert(
        &self,
        worktree: &Path,
        commit: &str,
        mainline: Option<u8>,
    ) -> Result<(), ForgeError> {
        let m = mainline.map(|m| m.to_string());
        let mut args = vec!["revert", "--no-edit"];
        if let Some(m) = m.as_deref() {
            args.extend(["-m", m]);
        }
        args.push(commit);
        self.git(worktree, &args).await?;
        Ok(())
    }

    /// Points a local branch at `commit` (keeps the revert commit reachable).
    pub async fn branch_force(
        &self,
        repo: &Path,
        name: &str,
        commit: &str,
    ) -> Result<(), ForgeError> {
        self.git(repo, &["branch", "-f", name, commit]).await?;
        Ok(())
    }

    /// Deletes a local branch; a missing branch is not an error (`git branch
    /// -D` exits 1 for it, verified on git 2.x macOS).
    pub async fn branch_delete(&self, repo: &Path, name: &str) -> Result<(), ForgeError> {
        match self.git(repo, &["branch", "-D", name]).await {
            Ok(_) | Err(ForgeError::Failed { code: Some(1), .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The commit `origin` holds for `branch`, if the branch exists there.
    pub async fn remote_branch_sha(
        &self,
        repo: &Path,
        branch: &str,
    ) -> Result<Option<String>, ForgeError> {
        let out = self
            .git(
                repo,
                &["ls-remote", "origin", &format!("refs/heads/{branch}")],
            )
            .await?;
        // ls-remote patterns tail-match, so accept only the exact ref.
        let want = format!("refs/heads/{branch}");
        Ok(out.lines().find_map(|l| {
            let (sha, name) = l.split_once('\t')?;
            (name.trim() == want).then(|| sha.trim().to_string())
        }))
    }

    /// Pushes an exact commit to a branch, never forcing.
    pub async fn push_sha(
        &self,
        repo: &Path,
        commit: &str,
        branch: &str,
    ) -> Result<(), ForgeError> {
        self.git(
            repo,
            &[
                "push",
                "--no-verify",
                "origin",
                &format!("{commit}:refs/heads/{branch}"),
            ],
        )
        .await?;
        Ok(())
    }

    /// Checks that validation commands have not changed the proposed revert.
    pub async fn clean(&self, worktree: &Path) -> Result<bool, ForgeError> {
        Ok(self
            .git(
                worktree,
                &["status", "--porcelain", "--untracked-files=all"],
            )
            .await?
            .is_empty())
    }

    /// Stages everything and commits. `None` when there is nothing to commit.
    pub async fn commit_all(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<String>, ForgeError> {
        self.git(worktree, &["add", "-A"]).await?;
        // `.provefab/` is in info/exclude; if a repo's own .gitignore re-includes it,
        // unstage it anyway. (A pathspec cannot do this: git refuses to name an
        // ignored path, even to exclude it.)
        let mut staged = self
            .git(worktree, &["diff", "--cached", "--name-only"])
            .await?;
        if staged.lines().any(|p| p.starts_with(".provefab/")) {
            self.git(worktree, &["reset", "-q", "--", ".provefab"])
                .await?;
            staged = self
                .git(worktree, &["diff", "--cached", "--name-only"])
                .await?;
        }
        if staged.trim().is_empty() {
            return Ok(None);
        }
        self.git(worktree, &["commit", "-q", "--no-verify", "-m", message])
            .await?;
        Ok(Some(self.git(worktree, &["rev-parse", "HEAD"]).await?))
    }

    /// Pushes the task branch. Runs with Provefab's own environment, which
    /// has the credentials workers never get (spec D23).
    pub async fn push(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError> {
        self.git(worktree, &["push", "--no-verify", "origin", branch])
            .await?;
        Ok(())
    }

    /// Updates `origin/*` so a new pass starts from the latest base (D48).
    pub async fn fetch(&self, repo: &Path) -> Result<(), ForgeError> {
        self.git(repo, &["fetch", "--quiet", "origin"]).await?;
        Ok(())
    }

    /// The worktree's current commit.
    pub async fn head(&self, worktree: &Path) -> Result<String, ForgeError> {
        self.git(worktree, &["rev-parse", "HEAD"]).await
    }

    /// Whether `ancestor` is an ancestor of `of` (both in `worktree`'s repo).
    pub async fn is_ancestor(
        &self,
        worktree: &Path,
        ancestor: &str,
        of: &str,
    ) -> Result<bool, ForgeError> {
        match self
            .git(worktree, &["merge-base", "--is-ancestor", ancestor, of])
            .await
        {
            Ok(_) => Ok(true),
            Err(ForgeError::Failed { code: Some(1), .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Resolves `rev` to its commit sha.
    pub async fn rev_parse(&self, repo: &Path, rev: &str) -> Result<String, ForgeError> {
        self.git(
            repo,
            &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
        )
        .await
    }

    /// `origin/<base>` when the repo has it (fetched), else the local `<base>`.
    pub async fn base_ref(&self, repo: &Path, base: &str) -> String {
        let remote = format!("refs/remotes/origin/{base}");
        match self
            .git(repo, &["rev-parse", "--verify", "--quiet", &remote])
            .await
        {
            Ok(_) => format!("origin/{base}"),
            Err(_) => base.to_string(),
        }
    }

    /// Every file the branch changed since `base`.
    pub async fn changed_files(
        &self,
        worktree: &Path,
        base: &str,
    ) -> Result<Vec<Change>, ForgeError> {
        let out = self
            .git(
                worktree,
                &["diff", "--name-status", &format!("{base}...HEAD")],
            )
            .await?;
        Ok(out.lines().filter_map(Change::parse).collect())
    }

    pub async fn diff(&self, worktree: &Path, base: &str) -> Result<String, ForgeError> {
        self.git(worktree, &["diff", &format!("{base}...HEAD")])
            .await
    }

    /// Added and removed lines per file since `base`; `None` for a binary file.
    pub async fn numstat(
        &self,
        worktree: &Path,
        base: &str,
    ) -> Result<Vec<(Option<u32>, Option<u32>, String)>, ForgeError> {
        let out = self
            .git(worktree, &["diff", "--numstat", &format!("{base}...HEAD")])
            .await?;
        Ok(parse_numstat(&out))
    }
}

/// Parses `git diff --numstat` output: one `added\tremoved\tpath` line per
/// file, or `-\t-\tpath` for a binary file.
fn parse_numstat(out: &str) -> Vec<(Option<u32>, Option<u32>, String)> {
    out.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?.parse().ok();
            let removed = parts.next()?.parse().ok();
            let path = parts.next()?.to_string();
            Some((added, removed, path))
        })
        .collect()
}

/// One line of `git diff --name-status`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    /// `A`, `M`, `D`, `R` (renamed), `C` (copied), ...
    pub status: char,
    pub path: String,
    /// The old path of a rename or copy.
    pub from: Option<String>,
}

impl Change {
    pub fn new(status: char, path: &str) -> Self {
        Self {
            status,
            path: path.to_string(),
            from: None,
        }
    }

    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split('\t');
        let status = parts.next()?.chars().next()?;
        let rest: Vec<&str> = parts.collect();
        match rest.as_slice() {
            [path] => Some(Change::new(status, path)),
            [from, path] => Some(Change {
                status,
                path: path.to_string(),
                from: Some(from.to_string()),
            }),
            _ => None,
        }
    }
}

fn is_test_path(path: &str) -> bool {
    let p = path.to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    p.split('/')
        .any(|seg| matches!(seg, "tests" | "test" | "__tests__" | "spec" | "specs"))
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_spec.rb")
}

/// Lines that declare a test, across the languages the gates usually run.
const TEST_MARKERS: &[&str] = &[
    "#[test]",
    "#[tokio::test]",
    "#[rstest]",
    "def test_",
    "func Test",
    "it(",
    "test(",
    "describe(",
    "@Test",
];

/// Whether the change adds or changes tests: a test file added or modified, or
/// an added line that declares a test (inline Rust tests live next to the code).
/// Used by merge policies that require a test change (landing L11).
pub fn adds_tests(changed: &[Change], diff: &str) -> bool {
    changed
        .iter()
        .any(|c| c.status != 'D' && is_test_path(&c.path))
        || diff.lines().any(|l| {
            l.strip_prefix('+')
                .filter(|_| !l.starts_with("+++"))
                .is_some_and(|added| {
                    let t = added.trim_start();
                    TEST_MARKERS.iter().any(|m| t.starts_with(m))
                })
        })
}

/// `gh repo view --json visibility -q .visibility` says the repo is public.
pub fn is_public_visibility(out: &str) -> bool {
    out.trim().eq_ignore_ascii_case("PUBLIC")
}

/// Markers that switch tests off or focus a single test (which skips the others).
/// Matched only in test files, and only at a word boundary (`xit(` must not
/// match `exit(`). `#[ignore` is also matched in any `.rs` file, since Rust
/// unit tests live next to the code.
const DISABLE_MARKERS: [&str; 17] = [
    "#[ignore",
    "@pytest.mark.skip",
    "@unittest.skip",
    "pytest.skip(",
    "it.skip(",
    "describe.skip(",
    "test.skip(",
    "it.only(",
    "describe.only(",
    "test.only(",
    "xit(",
    "xtest(",
    "xdescribe(",
    "t.Skip(",
    "t.SkipNow(",
    "@Disabled",
    "@Ignore",
];

/// `marker` occurs in `line` with no identifier character or `.` right before it.
fn has_marker(line: &str, marker: &str) -> bool {
    line.match_indices(marker).any(|(i, _)| {
        line[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'))
    })
}

/// Test files the change deletes or moves out of the test folders, and
/// test-disabling or focusing markers it adds (spec §3.4 item 6).
pub fn weakened_tests(changed: &[Change], diff: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for c in changed {
        if c.status == 'D' && is_test_path(&c.path) {
            found.push(format!("deleted test file `{}`", c.path));
        }
        if c.status == 'R'
            && let Some(from) = &c.from
            && is_test_path(from)
            && !is_test_path(&c.path)
        {
            found.push(format!("moved test file `{from}` to `{}`", c.path));
        }
    }
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.to_string();
            continue;
        }
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if line.starts_with("+++") {
            continue;
        }
        let in_test = is_test_path(&file);
        let is_rust = file.ends_with(".rs");
        for marker in DISABLE_MARKERS {
            let applies = in_test || (is_rust && marker == "#[ignore");
            if applies && has_marker(added, marker) {
                found.push(format!("`{file}` adds `{}`", marker.trim_end_matches('(')));
                break;
            }
        }
    }
    found
}

/// Prepends Provefab's fixed first line (D33) unless it is already there.
pub fn with_prefix(body: &str) -> String {
    if body.starts_with(BOT_PREFIX) {
        body.to_string()
    } else {
        format!("{BOT_PREFIX}\n\n{body}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    pub author: String,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Comment {
    pub author: String,
    /// GitHub's relation of the author to the repo: OWNER, MEMBER, COLLABORATOR, NONE, ...
    pub association: String,
    pub body: String,
    /// RFC 3339, as GitHub reports it; sorts correctly as a string.
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    pub state: PrState,
    pub comments: Vec<Comment>,
    /// The PR branch head commit, useful for diagnostics.
    pub head_sha: Option<String>,
    /// The squashed merge commit on the base branch.
    pub merge_sha: Option<String>,
    /// The PR target branch, used to verify the configured base.
    pub base_ref: Option<String>,
    /// A multi-commit PR cannot safely be undone by reverting only its last SHA.
    pub commit_count: Option<usize>,
}

/// The `comments` array of `gh issue view` / `gh pr view --json comments`.
fn parse_comments(v: &Value) -> Vec<Comment> {
    v.get("comments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Comment {
                author: c.pointer("/author/login")?.as_str()?.to_string(),
                association: c
                    .get("authorAssociation")
                    .and_then(Value::as_str)
                    .unwrap_or("NONE")
                    .to_string(),
                body: c.get("body")?.as_str()?.to_string(),
                created_at: c.get("createdAt")?.as_str()?.to_string(),
            })
        })
        .collect()
}

pub struct Gh {
    pub program: PathBuf,
}

/// `gh issue list --limit` for [`Gh::labeled_issues`]: high enough that a
/// backlog is never silently cut off, but still a single page.
const ISSUE_LIMIT: usize = 1000;

/// A warning for [`Gh::labeled_issues`] when `count` hits `ISSUE_LIMIT`: more
/// labelled issues may exist than were read.
fn issue_limit_warning(slug: &str, label: &str, count: usize) -> Option<String> {
    (count >= ISSUE_LIMIT).then(|| {
        format!(
            "provefab: {slug} has {ISSUE_LIMIT}+ open issues labelled {label}; only the first {ISSUE_LIMIT} are read"
        )
    })
}

impl Gh {
    async fn gh(&self, args: &[&str], stdin: Option<&str>) -> Result<String, ForgeError> {
        run(&self.program, None, args, stdin).await
    }

    fn json(&self, what: &str, text: &str) -> Result<Value, ForgeError> {
        serde_json::from_str(text).map_err(|e| ForgeError::Parse(what.to_string(), e.to_string()))
    }

    /// Open issues carrying `label` (spec §3.3).
    pub async fn labeled_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        let limit = ISSUE_LIMIT.to_string();
        let out = self
            .gh(
                &[
                    "issue",
                    "list",
                    "--repo",
                    slug,
                    "--label",
                    label,
                    "--state",
                    "open",
                    "--limit",
                    &limit,
                    "--json",
                    "number,title,body,url,author,labels",
                ],
                None,
            )
            .await?;
        let v = self.json("gh issue list", &out)?;
        let issues = v.as_array().cloned().unwrap_or_default();
        if let Some(warning) = issue_limit_warning(slug, label, issues.len()) {
            eprintln!("{warning}");
        }
        Ok(issues
            .iter()
            .filter_map(|i| {
                Some(Issue {
                    number: i.get("number")?.as_u64()?,
                    title: i.get("title")?.as_str()?.to_string(),
                    body: i
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    url: i.get("url")?.as_str()?.to_string(),
                    author: i.pointer("/author/login")?.as_str()?.to_string(),
                    labels: i
                        .get("labels")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect(),
                })
            })
            .collect())
    }

    /// One issue, whatever its labels (for `provefab add` and for re-reading the body).
    pub async fn issue(&self, slug: &str, number: u64) -> Result<Issue, ForgeError> {
        let n = number.to_string();
        let out = self
            .gh(
                &[
                    "issue",
                    "view",
                    &n,
                    "--repo",
                    slug,
                    "--json",
                    "number,title,body,url,author,labels",
                ],
                None,
            )
            .await?;
        let i = self.json("gh issue view", &out)?;
        // Title, URL and author identify the task: missing ones are an error, not "".
        let need = |v: Option<&Value>, what: &str| {
            v.and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ForgeError::Parse("gh issue view".into(), format!("no {what}")))
        };
        Ok(Issue {
            number: i.get("number").and_then(Value::as_u64).unwrap_or(number),
            title: need(i.get("title"), "title")?,
            body: i
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            url: need(i.get("url"), "url")?,
            author: need(i.pointer("/author/login"), "author")?,
            labels: i
                .get("labels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_string))
                .collect(),
        })
    }

    pub async fn comments(&self, slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        let n = number.to_string();
        let out = self
            .gh(
                &["issue", "view", &n, "--repo", slug, "--json", "comments"],
                None,
            )
            .await?;
        let v = self.json("gh issue view", &out)?;
        Ok(parse_comments(&v))
    }

    /// A PR's state and its comments (D52).
    pub async fn pr_status(&self, slug: &str, url: &str) -> Result<PrStatus, ForgeError> {
        let out = self
            .gh(
                &[
                    "pr",
                    "view",
                    url,
                    "--repo",
                    slug,
                    "--json",
                    "number,state,comments,reviews,headRefOid,mergeCommit,baseRefName,commits",
                ],
                None,
            )
            .await?;
        let v = self.json("gh pr view", &out)?;
        // Conversation comments, review bodies and inline review comments all
        // carry what a closing reviewer said (Plan 4 review I3).
        let mut comments = parse_comments(&v);
        comments.extend(
            v.get("reviews")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    let body = r.get("body")?.as_str()?.trim();
                    (!body.is_empty()).then(|| Comment {
                        author: r
                            .pointer("/author/login")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        association: r
                            .get("authorAssociation")
                            .and_then(Value::as_str)
                            .unwrap_or("NONE")
                            .to_string(),
                        body: body.to_string(),
                        created_at: r
                            .get("submittedAt")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                }),
        );
        if let Some(n) = v.get("number").and_then(Value::as_u64) {
            let path = format!("repos/{slug}/pulls/{n}/comments");
            let inline = self.gh(&["api", &path, "--paginate"], None).await?;
            let inline = self.json("gh api pulls comments", &inline)?;
            comments.extend(inline.as_array().into_iter().flatten().filter_map(|c| {
                let at = match (
                    c.get("path").and_then(Value::as_str),
                    c.get("line").and_then(Value::as_u64),
                ) {
                    (Some(p), Some(l)) => format!("{p}:{l}: "),
                    (Some(p), None) => format!("{p}: "),
                    _ => String::new(),
                };
                Some(Comment {
                    author: c.pointer("/user/login")?.as_str()?.to_string(),
                    association: c
                        .get("author_association")
                        .and_then(Value::as_str)
                        .unwrap_or("NONE")
                        .to_string(),
                    body: format!("{at}{}", c.get("body")?.as_str()?),
                    created_at: c
                        .get("created_at")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
            }));
        }
        let state = match v.get("state").and_then(Value::as_str) {
            Some("OPEN") => PrState::Open,
            Some("MERGED") => PrState::Merged,
            Some("CLOSED") => PrState::Closed,
            other => {
                return Err(ForgeError::Parse(
                    "gh pr view".into(),
                    format!("unknown state {other:?}"),
                ));
            }
        };
        Ok(PrStatus {
            state,
            comments,
            head_sha: v
                .get("headRefOid")
                .and_then(Value::as_str)
                .map(str::to_string),
            merge_sha: v
                .pointer("/mergeCommit/oid")
                .and_then(Value::as_str)
                .map(str::to_string),
            base_ref: v
                .get("baseRefName")
                .and_then(Value::as_str)
                .map(str::to_string),
            commit_count: v.get("commits").and_then(Value::as_array).map(Vec::len),
        })
    }

    /// Squash-merges the PR and deletes its branch, only if its head is still
    /// `head`, the commit Provefab checked (D49, Plan 4 review I6).
    pub async fn pr_merge(&self, slug: &str, url: &str, head: &str) -> Result<(), ForgeError> {
        self.gh(
            &[
                "pr",
                "merge",
                url,
                "--repo",
                slug,
                "--squash",
                "--delete-branch",
                "--match-head-commit",
                head,
            ],
            None,
        )
        .await?;
        Ok(())
    }

    /// Whether the repository is public: anyone can then write the issue text
    /// agents read (landing L11: no auto-merge on public repos by default).
    pub async fn repo_is_public(&self, slug: &str) -> Result<bool, ForgeError> {
        let out = self
            .gh(
                &[
                    "repo",
                    "view",
                    slug,
                    "--json",
                    "visibility",
                    "-q",
                    ".visibility",
                ],
                None,
            )
            .await?;
        Ok(is_public_visibility(&out))
    }

    /// Whether the issue is open (a merged fix whose issue was reopened, D52).
    pub async fn issue_open(&self, slug: &str, number: u64) -> Result<bool, ForgeError> {
        let n = number.to_string();
        let out = self
            .gh(
                &["issue", "view", &n, "--repo", slug, "--json", "state"],
                None,
            )
            .await?;
        let v = self.json("gh issue view", &out)?;
        match v.get("state").and_then(Value::as_str) {
            Some("OPEN") => Ok(true),
            Some("CLOSED") => Ok(false),
            other => Err(ForgeError::Parse(
                "gh issue view".into(),
                format!("unknown state {other:?}"),
            )),
        }
    }

    /// Posts a comment; the body always gets Provefab's first line.
    pub async fn comment(&self, slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        let n = number.to_string();
        self.gh(
            &["issue", "comment", &n, "--repo", slug, "--body-file", "-"],
            Some(&with_prefix(body)),
        )
        .await?;
        Ok(())
    }

    /// Posts a bot comment on a pull request.
    pub async fn pr_comment(&self, slug: &str, url: &str, body: &str) -> Result<(), ForgeError> {
        self.gh(
            &["pr", "comment", url, "--repo", slug, "--body-file", "-"],
            Some(&with_prefix(body)),
        )
        .await?;
        Ok(())
    }

    pub async fn edit_labels(
        &self,
        slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), ForgeError> {
        let n = number.to_string();
        let mut args = vec!["issue", "edit", n.as_str(), "--repo", slug];
        let add = add.join(",");
        let remove = remove.join(",");
        if !add.is_empty() {
            args.extend(["--add-label", add.as_str()]);
        }
        if !remove.is_empty() {
            args.extend(["--remove-label", remove.as_str()]);
        }
        self.gh(&args, None).await?;
        Ok(())
    }

    /// Clones `slug` into `dest` with `gh`'s own authentication (D48).
    pub async fn repo_clone(&self, slug: &str, dest: &Path) -> Result<(), ForgeError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ForgeError::Parse(parent.display().to_string(), e.to_string()))?;
        }
        let d = dest.display().to_string();
        self.gh(&["repo", "clone", slug, &d, "--", "--quiet"], None)
            .await?;
        Ok(())
    }

    /// Creates the label, or updates its colour and description if it exists.
    pub async fn ensure_label(
        &self,
        slug: &str,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), ForgeError> {
        self.gh(
            &[
                "label",
                "create",
                name,
                "--repo",
                slug,
                "--color",
                color,
                "--description",
                description,
                "--force",
            ],
            None,
        )
        .await?;
        Ok(())
    }

    /// Opens the pull request and returns its URL.
    pub async fn pr_create(
        &self,
        slug: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        // A restart between creating the PR and recording it must not try to open it twice.
        // `--head` matches the branch name only, so a fork's PR on a same-named branch
        // would also match: reuse only a PR from this repository aimed at `base`.
        let open = self
            .gh(
                &[
                    "pr",
                    "list",
                    "--repo",
                    slug,
                    "--head",
                    head,
                    "--state",
                    "open",
                    "--json",
                    "url,isCrossRepository,baseRefName",
                ],
                None,
            )
            .await?;
        let existing = self.json("gh pr list", &open)?;
        let ours = existing.as_array().into_iter().flatten().find(|p| {
            p.get("isCrossRepository").and_then(Value::as_bool) == Some(false)
                && p.get("baseRefName").and_then(Value::as_str) == Some(base)
        });
        if let Some(url) = ours.and_then(|p| p.get("url")).and_then(Value::as_str) {
            return Ok(url.to_string());
        }
        let out = self
            .gh(
                &[
                    "pr",
                    "create",
                    "--repo",
                    slug,
                    "--head",
                    head,
                    "--base",
                    base,
                    "--title",
                    title,
                    "--body-file",
                    "-",
                ],
                Some(body),
            )
            .await?;
        out.lines()
            .rev()
            .find(|l| l.starts_with("https://"))
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Parse("gh pr create".into(), out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn branch_names_are_short_and_safe() {
        assert_eq!(
            branch_name(42, "Fix: crash when `config` is empty!"),
            "provefab/42-fix-crash-when-config-is-empty"
        );
        let long = branch_name(7, &"word ".repeat(40));
        assert!(long.len() <= 60, "{long}");
        assert!(!long.ends_with('-'), "{long}");
        assert_eq!(branch_name(3, "日本語"), "provefab/3-");
    }

    /// Comments posted before the rename carry the old prefix; they are still
    /// the bot's, never a human reply (plan 5 review focus 1).
    #[test]
    fn legacy_bot_comments_are_still_the_bots() {
        assert!(is_bot_comment(&format!("{BOT_PREFIX}\n\nhello")));
        assert!(is_bot_comment(&format!(
            "{LEGACY_BOT_PREFIX}\n\nWhich version?"
        )));
        assert!(!is_bot_comment("It is version 2."));
        assert!(BOT_PREFIX.contains("Provefab"));
    }

    /// Auto-merge policy (landing L11): a change must add or change tests,
    /// in a test file or inline (Rust `#[test]` next to the code).
    #[test]
    fn adds_tests_sees_test_files_and_inline_tests() {
        let lib = [Change::new('M', "src/lib.rs")];
        assert!(!adds_tests(&lib, "+pub fn range() {}\n"));
        assert!(adds_tests(&lib, "+    #[test]\n+    fn empty() {}\n"));
        assert!(adds_tests(&lib, "+#[tokio::test]\n"));
        assert!(adds_tests(&[Change::new('A', "tests/range.rs")], ""));
        assert!(adds_tests(&[Change::new('M', "app/foo.test.ts")], ""));
        assert!(adds_tests(
            &[Change::new('M', "app.py")],
            "+def test_range():\n"
        ));
        assert!(adds_tests(&[Change::new('M', "a_test.go")], ""));
        // Deleting a test file is not adding one.
        assert!(!adds_tests(&[Change::new('D', "tests/old.rs")], ""));
        // A removed test line is not an added one.
        assert!(!adds_tests(&lib, "-    #[test]\n"));
    }

    #[test]
    fn visibility_parses_gh_output() {
        assert!(is_public_visibility("PUBLIC\n"));
        assert!(!is_public_visibility("PRIVATE"));
        assert!(!is_public_visibility("INTERNAL"));
    }

    #[test]
    fn weakened_tests_finds_deleted_test_files_and_disabled_tests() {
        let changed = vec![
            Change::new('D', "tests/parser.rs"),
            Change::new('D', "src/old.rs"),
            Change::new('M', "src/lib.rs"),
            Change::new('D', "web/app.test.ts"),
        ];
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@\n+    #[ignore]\n+    fn flaky() {}\n-    #[ignore]\n+++ b/py/test_api.py\n+@pytest.mark.skip(reason=\"later\")\n";
        let found = weakened_tests(&changed, diff);
        assert_eq!(
            found,
            vec![
                "deleted test file `tests/parser.rs`",
                "deleted test file `web/app.test.ts`",
                "`src/lib.rs` adds `#[ignore`",
                "`py/test_api.py` adds `@pytest.mark.skip`",
            ]
        );
        assert!(weakened_tests(&[Change::new('M', "src/lib.rs")], "+fn ok() {}\n").is_empty());
    }

    #[test]
    fn prefix_is_added_once() {
        let once = with_prefix("Need the stack trace.");
        assert!(once.starts_with(BOT_PREFIX) && once.ends_with("Need the stack trace."));
        assert_eq!(with_prefix(&once), once);
    }

    fn git() -> Git {
        Git {
            program: "git".into(),
        }
    }

    async fn sh(cwd: &Path, script: &str) {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(cwd)
            // Hermetic: the gates' no-push `GIT_CONFIG_*` must not reach test repos.
            .env_remove("GIT_CONFIG_COUNT")
            .status()
            .await
            .unwrap()
            .success();
        assert!(ok, "{script}");
    }

    /// A repo with one commit on `main` and a bare `origin`.
    async fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        sh(
            dir.path(),
            "git init -q --bare origin.git && git init -q -b main repo && cd repo \
             && git config user.email t@t && git config user.name t \
             && echo a > a.txt && mkdir tests && echo t > tests/t.rs && git add -A && git commit -q -m init \
             && git remote add origin ../origin.git && git push -q origin main",
        )
        .await;
        let repo = dir.path().join("repo");
        (dir, repo)
    }

    #[tokio::test]
    async fn worktree_commit_push_and_changed_files() {
        let (dir, repo) = repo().await;
        let wt = dir.path().join("wt");
        let g = git();
        g.worktree_add(&repo, &wt, "provefab/1-x", "main")
            .await
            .unwrap();
        g.worktree_add(&repo, &wt, "provefab/1-x", "main")
            .await
            .unwrap(); // idempotent
        assert_eq!(g.commit_all(&wt, "nothing").await.unwrap(), None);
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        std::fs::remove_file(wt.join("tests/t.rs")).unwrap();
        let sha = g
            .commit_all(&wt, "provefab: fix #1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sha.len(), 40);
        g.push(&wt, "provefab/1-x").await.unwrap();
        let remote = run(
            Path::new("git"),
            Some(&dir.path().join("origin.git")),
            &["branch", "--list"],
            None,
        )
        .await
        .unwrap();
        assert!(remote.contains("provefab/1-x"), "{remote}");
        let mut changed = g.changed_files(&wt, "main").await.unwrap();
        changed.sort();
        assert_eq!(
            changed,
            vec![Change::new('A', "b.txt"), Change::new('D', "tests/t.rs")]
        );
        g.worktree_remove(&repo, &wt).await.unwrap();
        assert!(!wt.exists());
        // Reattaching an existing branch (a resumed task) works too.
        g.worktree_add(&repo, &wt, "provefab/1-x", "main")
            .await
            .unwrap();
        assert!(wt.join("b.txt").exists());
    }

    /// Review C1: a gate (husky's `prepare`) can point `core.hooksPath` at a
    /// directory the agent writes; Provefab's commit and push then run with
    /// full credentials and must never execute those hooks.
    #[tokio::test]
    async fn review_c1_the_provefab_never_runs_repo_hooks() {
        let (dir, repo) = repo().await;
        let wt = dir.path().join("wt");
        let g = git();
        g.worktree_add(&repo, &wt, "provefab/2-x", "main")
            .await
            .unwrap();
        let marker = dir.path().join("hook-ran");
        let hooks = wt.join(".husky");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["pre-commit", "commit-msg", "pre-push", "post-commit"] {
            let p = hooks.join(hook);
            std::fs::write(
                &p,
                format!("#!/bin/sh\necho {hook} >> {}\n", marker.display()),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        sh(&repo, "git config core.hooksPath .husky").await;
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        g.commit_all(&wt, "provefab: x").await.unwrap().unwrap();
        g.push(&wt, "provefab/2-x").await.unwrap();
        assert!(
            !marker.exists(),
            "{}",
            std::fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    /// Seen 2026-09-26: two tasks creating worktrees from `origin/main` at once
    /// both wrote upstream tracking into `.git/config`, and one failed on its
    /// lock. Provefab never needs tracking: nothing may write that file.
    #[tokio::test]
    async fn worktrees_and_pushes_never_write_tracking_config() {
        let (dir, repo) = repo().await;
        let g = git();
        g.fetch(&repo).await.unwrap();
        let wt = dir.path().join("wt-track");
        g.worktree_add(&repo, &wt, "provefab/3-x", "origin/main")
            .await
            .unwrap();
        std::fs::write(wt.join("c.txt"), "c").unwrap();
        g.commit_all(&wt, "x").await.unwrap().unwrap();
        g.push(&wt, "provefab/3-x").await.unwrap();
        let config = std::fs::read_to_string(repo.join(".git/config")).unwrap();
        assert!(!config.contains("[branch \"provefab/3-x\"]"), "{config}");
    }

    #[test]
    fn parse_numstat_reads_binary_rows_as_none() {
        assert_eq!(
            parse_numstat("1\t2\ta.rs\n-\t-\tbin.dat"),
            vec![
                (Some(1), Some(2), "a.rs".to_string()),
                (None, None, "bin.dat".to_string()),
            ]
        );
        assert_eq!(parse_numstat(""), vec![]);
    }

    /// Issue #8: `git diff` line filtering undercounts real content lines that
    /// happen to start with `+++`/`---`, and misses binary files entirely.
    /// `numstat` must count every changed line and flag binary files.
    #[tokio::test]
    async fn numstat_counts_plus_plus_lines_and_binary_files() {
        let (_dir, repo) = repo().await;
        sh(
            &repo,
            "git checkout -q -b x \
             && printf '++i;\\n' > c.txt \
             && printf '++i;\\n' > a.txt \
             && printf '\\0\\x01\\x02\\xff' > bin.dat \
             && git add -A && git commit -q -m x",
        )
        .await;
        let g = git();
        let mut rows = g.numstat(&repo, "main").await.unwrap();
        rows.sort_by(|a, b| a.2.cmp(&b.2));
        assert_eq!(
            rows,
            vec![
                (Some(1), Some(1), "a.txt".to_string()),
                (None, None, "bin.dat".to_string()),
                (Some(1), Some(0), "c.txt".to_string()),
            ]
        );
        assert_eq!(crate::pipeline::numstat_lines(&rows), u32::MAX);
        // Without the binary file, the text rows (including the `+++i;`
        // and `---i;`-shaped lines a diff-line counter would have skipped) sum to 3.
        let text_rows: Vec<_> = rows
            .into_iter()
            .filter(|(_, _, path)| path != "bin.dat")
            .collect();
        assert_eq!(crate::pipeline::numstat_lines(&text_rows), 3);
    }

    #[tokio::test]
    async fn fetch_and_base_ref_prefer_origin() {
        let (dir, repo) = repo().await;
        let g = git();
        // No remote-tracking ref yet in a fresh clone-less repo: the local base.
        sh(
            &repo,
            "git update-ref -d refs/remotes/origin/main 2>/dev/null; true",
        )
        .await;
        assert_eq!(g.base_ref(&repo, "main").await, "main");
        g.fetch(&repo).await.unwrap();
        assert_eq!(g.base_ref(&repo, "main").await, "origin/main");
        // A repo without origin: fetch fails, base stays local.
        let bare = dir.path().join("lonely");
        sh(dir.path(), "git init -q -b main lonely").await;
        assert!(g.fetch(&bare).await.is_err());
        assert_eq!(g.base_ref(&bare, "main").await, "main");
    }

    #[tokio::test]
    async fn git_failures_carry_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = git().commit_all(dir.path(), "x").await.unwrap_err();
        assert!(
            matches!(&err, ForgeError::Failed { stderr, .. } if stderr.contains("not a git repository")),
            "{err}"
        );
    }

    /// A fake `gh` that logs its argv and stdin, then prints `out`.
    fn fake_gh(dir: &Path, out: &str) -> Gh {
        std::fs::write(dir.join("out.txt"), out).unwrap();
        if !dir.join("list.txt").exists() {
            std::fs::write(dir.join("list.txt"), "[]").unwrap();
        }
        if !dir.join("api.txt").exists() {
            std::fs::write(dir.join("api.txt"), "[]").unwrap();
        }
        // `gh pr list` answers from list.txt (no open PR by default), `gh api`
        // from api.txt; everything else from out.txt.
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do echo \"ARG $a\"; done > {log}\ncat > {stdin}\n\
             if [ \"$1 $2\" = \"pr list\" ]; then cat {list}; elif [ \"$1\" = api ]; then cat {api}; else cat {out}; fi\n",
            log = dir.join("log.txt").display(),
            stdin = dir.join("stdin.txt").display(),
            out = dir.join("out.txt").display(),
            list = dir.join("list.txt").display(),
            api = dir.join("api.txt").display()
        );
        let bin = dir.join("gh");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Gh { program: bin }
    }

    #[test]
    fn issue_limit_warning_only_fires_at_the_limit() {
        assert_eq!(issue_limit_warning("o/r", "provefab", 999), None);
        assert_eq!(
            issue_limit_warning("o/r", "provefab", 1000),
            Some(
                "provefab: o/r has 1000+ open issues labelled provefab; only the first 1000 are read"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn gh_lists_labeled_issues() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            r#"[{"number":7,"title":"Crash","body":"stack","url":"https://github.com/o/r/issues/7","author":{"login":"alice"},"labels":[{"name":"provefab"}]}]"#,
        );
        let issues = gh.labeled_issues("o/r", "provefab").await.unwrap();
        assert_eq!(
            issues,
            vec![Issue {
                number: 7,
                title: "Crash".into(),
                body: "stack".into(),
                url: "https://github.com/o/r/issues/7".into(),
                author: "alice".into(),
                labels: vec!["provefab".into()],
            }]
        );
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(
            log.contains("ARG --label\nARG provefab\nARG --state\nARG open\nARG --limit\nARG 1000"),
            "{log}"
        );
    }

    /// A backlog of exactly 1000 open issues is read in full, and warns that
    /// more may exist beyond the limit.
    #[tokio::test]
    async fn gh_warns_when_labeled_issues_hit_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = String::from("[");
        for n in 0..1000 {
            if n > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                r#"{{"number":{n},"title":"t","body":"","url":"https://github.com/o/r/issues/{n}","author":{{"login":"a"}},"labels":[]}}"#
            ));
        }
        out.push(']');
        let gh = fake_gh(dir.path(), &out);
        let issues = gh.labeled_issues("o/r", "provefab").await.unwrap();
        assert_eq!(issues.len(), 1000);
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(log.contains("ARG --limit\nARG 1000"), "{log}");
    }

    #[tokio::test]
    async fn gh_comments_are_prefixed_and_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "");
        gh.comment("o/r", 7, "Which version?").await.unwrap();
        let sent = std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap();
        assert!(
            sent.starts_with(BOT_PREFIX) && sent.ends_with("Which version?"),
            "{sent}"
        );
        let gh = fake_gh(
            dir.path(),
            r#"{"comments":[{"author":{"login":"bob"},"authorAssociation":"COLLABORATOR","body":"v2","createdAt":"2026-09-24T10:00:00Z"}]}"#,
        );
        assert_eq!(
            gh.comments("o/r", 7).await.unwrap(),
            vec![Comment {
                author: "bob".into(),
                association: "COLLABORATOR".into(),
                body: "v2".into(),
                created_at: "2026-09-24T10:00:00Z".into(),
            }]
        );
    }

    #[tokio::test]
    async fn gh_reads_one_issue_whatever_its_labels() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            r#"{"number":9,"title":"T","body":null,"url":"https://github.com/o/r/issues/9","author":{"login":"bob"},"labels":[]}"#,
        );
        let issue = gh.issue("o/r", 9).await.unwrap();
        assert_eq!(
            (issue.number, issue.body.as_str(), issue.author.as_str()),
            (9, "", "bob")
        );
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(
            log.starts_with("ARG issue\nARG view\nARG 9\nARG --repo\nARG o/r"),
            "{log}"
        );
        let bad = fake_gh(dir.path(), "[]");
        assert!(bad.issue("o/r", 9).await.is_err());
        let blank = fake_gh(
            dir.path(),
            r#"{"number":9,"title":"","url":"https://github.com/o/r/issues/9","author":{"login":"bob"}}"#,
        );
        assert!(blank.issue("o/r", 9).await.is_err());
    }

    #[tokio::test]
    async fn gh_repo_clone_runs_gh_with_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "");
        let dest = dir.path().join("repos").join("o").join("r");
        gh.repo_clone("o/r", &dest).await.unwrap();
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert_eq!(
            log,
            format!(
                "ARG repo\nARG clone\nARG o/r\nARG {}\nARG --\nARG --quiet\n",
                dest.display()
            )
        );
        assert!(dest.parent().unwrap().is_dir());
    }

    #[tokio::test]
    async fn gh_pr_status_and_issue_state() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            r#"{"state":"CLOSED","comments":[{"author":{"login":"alice"},"authorAssociation":"OWNER","body":"use a running mean","createdAt":"2026-09-25T10:00:00Z"}]}"#,
        );
        let s = gh
            .pr_status("o/r", "https://github.com/o/r/pull/6")
            .await
            .unwrap();
        assert_eq!(s.state, PrState::Closed);
        assert_eq!(s.comments[0].body, "use a running mean");
        // Review bodies and inline review comments count too (Plan 4 review I3).
        std::fs::write(
            dir.path().join("api.txt"),
            r#"[{"user":{"login":"bob"},"author_association":"MEMBER","body":"this breaks the API","created_at":"2026-09-25T10:01:00Z","path":"src/lib.rs","line":3}]"#,
        )
        .unwrap();
        let gh = fake_gh(
            dir.path(),
            r#"{"number":6,"state":"CLOSED","comments":[],"reviews":[{"author":{"login":"carol"},"authorAssociation":"COLLABORATOR","body":"Request changes: wrong approach","state":"CHANGES_REQUESTED","submittedAt":"2026-09-25T10:02:00Z"},{"author":{"login":"dan"},"authorAssociation":"MEMBER","body":"","state":"APPROVED","submittedAt":"2026-09-25T10:03:00Z"}]}"#,
        );
        let s = gh
            .pr_status("o/r", "https://github.com/o/r/pull/6")
            .await
            .unwrap();
        let bodies: Vec<&str> = s.comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec![
                "Request changes: wrong approach",
                "src/lib.rs:3: this breaks the API"
            ]
        );
        assert_eq!(s.comments[1].association, "MEMBER");
        std::fs::write(dir.path().join("api.txt"), "[]").unwrap();
        let gh = fake_gh(
            dir.path(),
            r#"{"state":"MERGED","comments":[],"headRefOid":"branch-head","mergeCommit":{"oid":"merged-sha"},"baseRefName":"main","commits":[{"oid":"branch-head"}]}"#,
        );
        let merged = gh.pr_status("o/r", "u").await.unwrap();
        assert_eq!(merged.state, PrState::Merged);
        assert_eq!(merged.head_sha.as_deref(), Some("branch-head"));
        assert_eq!(merged.merge_sha.as_deref(), Some("merged-sha"));
        assert_eq!(merged.base_ref.as_deref(), Some("main"));
        assert_eq!(merged.commit_count, Some(1));
        let gh = fake_gh(dir.path(), r#"{"state":"DRAFT?","comments":[]}"#);
        assert!(gh.pr_status("o/r", "u").await.is_err());
        let gh = fake_gh(dir.path(), "");
        gh.pr_merge("o/r", "https://github.com/o/r/pull/6", "abc123")
            .await
            .unwrap();
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert_eq!(
            log,
            "ARG pr\nARG merge\nARG https://github.com/o/r/pull/6\nARG --repo\nARG o/r\nARG --squash\nARG --delete-branch\nARG --match-head-commit\nARG abc123\n"
        );
        let gh = fake_gh(dir.path(), r#"{"state":"OPEN"}"#);
        assert!(gh.issue_open("o/r", 5).await.unwrap());
        let gh = fake_gh(dir.path(), r#"{"state":"CLOSED"}"#);
        assert!(!gh.issue_open("o/r", 5).await.unwrap());
    }

    #[tokio::test]
    async fn gh_ensure_label_creates_or_updates() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "");
        gh.ensure_label("o/r", "provefab:in-pr", "0e8a16", "A PR is open")
            .await
            .unwrap();
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(
            log.starts_with("ARG label\nARG create\nARG provefab:in-pr\nARG --repo\nARG o/r")
                && log.ends_with("ARG --force\n"),
            "{log}"
        );
    }

    #[tokio::test]
    async fn gh_labels_and_pr_create() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            "Creating pull request\nhttps://github.com/o/r/pull/12\n",
        );
        let url = gh
            .pr_create("o/r", "provefab/7-crash", "main", "Fix crash", "body")
            .await
            .unwrap();
        assert_eq!(url, "https://github.com/o/r/pull/12");
        gh.edit_labels("o/r", 7, &["provefab:in-pr"], &["provefab"])
            .await
            .unwrap();
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(
            log.contains("ARG --add-label\nARG provefab:in-pr\nARG --remove-label\nARG provefab"),
            "{log}"
        );
        let bad = fake_gh(dir.path(), "not json");
        assert!(matches!(
            bad.labeled_issues("o/r", "provefab").await,
            Err(ForgeError::Parse(..))
        ));
    }

    /// Final review I5: markers are word-bounded and apply to test files only
    /// (`#[ignore` also in any `.rs` file, where Rust unit tests live).
    #[test]
    fn review_i5_markers_are_word_bounded_and_scoped_to_tests() {
        let diff = "+++ b/src/lib.rs\n+    let x = it.skip(1);\n+    std::process::exit(1);\n\
                    +++ b/tests/api.rs\n+    std::process::exit(1);\n\
                    +++ b/web/app.ts\n+  process.exit(2);\n\
                    +++ b/web/app.test.ts\n+  it.only('focus', () => {});\n+  xtest('later', () => {});\n\
                    +++ b/pkg/foo_test.go\n+\tt.SkipNow()\n";
        assert_eq!(
            weakened_tests(&[], diff),
            vec![
                "`web/app.test.ts` adds `it.only`",
                "`web/app.test.ts` adds `xtest`",
                "`pkg/foo_test.go` adds `t.SkipNow`",
            ]
        );
    }

    /// Final review I5: a test file renamed out of the test folders is flagged.
    #[test]
    fn review_i5_test_moved_out_of_tests_is_flagged() {
        let moved = Change::parse("R100\ttests/parser.rs\tsrc/parser_old.rs").unwrap();
        assert_eq!(moved.from.as_deref(), Some("tests/parser.rs"));
        assert_eq!(
            weakened_tests(&[moved], ""),
            vec!["moved test file `tests/parser.rs` to `src/parser_old.rs`"]
        );
        let kept = Change::parse("R090\ttests/a.rs\ttests/b.rs").unwrap();
        assert!(weakened_tests(&[kept], "").is_empty());
    }

    /// Final review I6: `.provefab/` (stage outputs) is never committed.
    #[tokio::test]
    async fn review_i6_provefab_dir_is_never_committed() {
        let (dir, repo) = repo().await;
        let wt = dir.path().join("wt6");
        let g = git();
        g.worktree_add(&repo, &wt, "provefab/6-x", "main")
            .await
            .unwrap();
        std::fs::create_dir(wt.join(".provefab")).unwrap();
        std::fs::write(wt.join(".provefab/plan.md"), "plan").unwrap();
        std::fs::write(wt.join("n.txt"), "n").unwrap();
        g.commit_all(&wt, "provefab: 6").await.unwrap().unwrap();
        assert_eq!(
            g.changed_files(&wt, "main").await.unwrap(),
            vec![Change::new('A', "n.txt")]
        );
    }

    /// Final review (minor, promoted): a restart after the PR was created reuses it.
    #[tokio::test]
    async fn review_pr_create_reuses_an_open_pr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("list.txt"),
            r#"[{"url":"https://github.com/o/r/pull/3","isCrossRepository":false,"baseRefName":"main"}]"#,
        )
        .unwrap();
        let gh = fake_gh(dir.path(), "should not be called");
        let url = gh
            .pr_create("o/r", "provefab/7-crash", "main", "Fix", "body")
            .await
            .unwrap();
        assert_eq!(url, "https://github.com/o/r/pull/3");
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert!(log.starts_with("ARG pr\nARG list"), "{log}");
    }

    /// Final review (minor, promoted): resume after the worktree directory was deleted
    /// by hand, and refuse a directory that is on another branch.
    #[tokio::test]
    async fn review_worktree_add_recovers_and_checks_the_branch() {
        let (dir, repo) = repo().await;
        let wt = dir.path().join("wt7");
        let g = git();
        g.worktree_add(&repo, &wt, "provefab/7-x", "main")
            .await
            .unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        g.worktree_add(&repo, &wt, "provefab/7-x", "main")
            .await
            .unwrap();
        assert!(wt.join("a.txt").exists());
        let err = g
            .worktree_add(&repo, &wt, "provefab/8-y", "main")
            .await
            .unwrap_err();
        assert!(matches!(err, ForgeError::WrongWorktree { .. }), "{err}");
        assert!(err.is_permanent(), "{err}");
    }

    /// A worktree on the wrong branch cannot fix itself by waiting (issue #10).
    #[test]
    fn wrong_worktree_is_permanent() {
        let err = ForgeError::WrongWorktree {
            path: "wt".into(),
            actual: "other".into(),
            wanted: "main".into(),
        };
        assert!(err.is_permanent());
    }

    /// A missing issue cannot fix itself by waiting (issue #10).
    #[test]
    fn missing_issue_is_permanent() {
        let err = ForgeError::Failed {
            program: "gh".into(),
            args: "issue view 7".into(),
            code: Some(1),
            stderr: "GraphQL: Could not resolve to an issue or pull request with the number of 7."
                .into(),
        };
        assert!(err.is_permanent());
    }

    /// A missing or inaccessible repository cannot fix itself by waiting (issue #10).
    #[test]
    fn missing_repository_via_gh_is_permanent() {
        let err = ForgeError::Failed {
            program: "gh".into(),
            args: "repo clone o/r".into(),
            code: Some(1),
            stderr: "GraphQL: Could not resolve to a Repository with the name 'o/r'.".into(),
        };
        assert!(err.is_permanent());
    }

    /// A missing or private git remote cannot fix itself by waiting (issue #10).
    #[test]
    fn missing_repository_via_git_is_permanent() {
        let err = ForgeError::Failed {
            program: "git".into(),
            args: "push origin provefab/7-x".into(),
            code: Some(128),
            stderr: "remote: Repository not found.\nfatal: repository 'https://x' not found".into(),
        };
        assert!(err.is_permanent());
    }

    /// A transient server error is not permanent: waiting may clear it (issue #10).
    #[test]
    fn an_http_502_is_not_permanent() {
        let err = ForgeError::Failed {
            program: "gh".into(),
            args: "issue view 7".into(),
            code: Some(1),
            stderr: "HTTP 502: Bad Gateway".into(),
        };
        assert!(!err.is_permanent());
    }

    /// A timeout is not permanent: waiting may clear it (issue #10).
    #[test]
    fn a_timeout_is_not_permanent() {
        let err = ForgeError::Timeout {
            program: "git".into(),
            args: "push".into(),
            secs: 600,
        };
        assert!(!err.is_permanent());
    }

    /// Final review (minor, promoted): git and gh never wait on a terminal prompt and
    /// never hang Provefab.
    #[tokio::test]
    async fn review_commands_time_out_and_never_prompt() {
        let started = std::time::Instant::now();
        let err = run_with_timeout(
            Path::new("sleep"),
            None,
            &["5"],
            None,
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ForgeError::Timeout { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
        let prompt = run(
            Path::new("sh"),
            None,
            &["-c", "echo $GIT_TERMINAL_PROMPT"],
            None,
        )
        .await
        .unwrap();
        assert_eq!(prompt, "0");
    }

    /// Security review: `gh pr list --head` matches the branch name only, so a PR
    /// from a fork on a branch called `provefab/7-...`, or one aimed at another
    /// base, must never be taken as Provefab's own PR.
    #[tokio::test]
    async fn pr_create_never_reuses_a_fork_or_other_base_pr() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("list.txt"),
            r#"[{"url":"https://github.com/o/r/pull/4","isCrossRepository":true,"baseRefName":"main"},
                {"url":"https://github.com/o/r/pull/5","isCrossRepository":false,"baseRefName":"release"},
                {"url":"https://github.com/o/r/pull/6"}]"#,
        )
        .unwrap();
        let gh = fake_gh(dir.path(), "https://github.com/o/r/pull/12\n");
        let url = gh
            .pr_create("o/r", "provefab/7-crash", "main", "Fix", "body")
            .await
            .unwrap();
        assert_eq!(url, "https://github.com/o/r/pull/12");
    }
}
