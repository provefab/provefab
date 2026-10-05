//! What no stage may read in Provefab's home (Linux final review, finding 1;
//! R3). Agents run as your account, so file permissions do not keep them away
//! from the credentials file, the licence, the database or the workers'
//! sign-ins: this check does, in every stage. Like the rest of the guard it is
//! a tripwire, not a sandbox: a script the agent writes and then runs is not
//! read.

use std::path::{Component, Path, PathBuf};

use super::paths::resolve_existing_prefix;

/// Files directly in the home that hold a secret.
const SECRET_FILES: [&str; 2] = ["credentials.toml", "license.key"];
/// The database and its `-wal`, `-shm` and `-journal` companions.
const DATABASE: &str = "provefab.db";
/// The workers' sign-in directories (Claude Code keeps `.credentials.json`
/// there on Linux, Codex `auth.json`).
const SECRET_DIRS: [&str; 4] = ["claude", "claude-api", "codex", "codex-api"];
/// Names that give a secret away when a word cannot be resolved (a variable,
/// a glob, a directory the guard cannot follow).
const SECRET_NAMES: [&str; 4] = [
    "credentials.toml",
    "license.key",
    "provefab.db",
    ".credentials.json",
];
/// What the shell would still expand once `~`, `$HOME` and `$PROVEFAB_HOME`
/// are replaced.
const EXPANSION: [char; 7] = ['$', '*', '?', '[', '{', '`', '~'];

const WHY: &str =
    "Provefab's credentials, licence, database and worker sign-ins are not readable by workers";

/// The Provefab homes to protect and the user's home, for `~` and `$HOME`.
pub struct Protected {
    pub(super) homes: Vec<PathBuf>,
    user_home: Option<PathBuf>,
}

impl Protected {
    /// The home from `PROVEFAB_HOME` (else `~/.provefab`) and, when the task's
    /// worktree is `<home>/worktrees/<id>`, that home too: the stage's
    /// environment may differ from the service's.
    pub fn from_env(root: &Path) -> Self {
        let mut homes = vec![crate::paths::Paths::from_env().home];
        if let Some(parent) = root.parent()
            && parent.file_name().is_some_and(|n| n == "worktrees")
            && let Some(home) = parent.parent()
        {
            homes.push(home.to_path_buf());
        }
        Self::new(homes, std::env::var_os("HOME").map(PathBuf::from))
    }

    pub fn new(homes: Vec<PathBuf>, user_home: Option<PathBuf>) -> Self {
        let mut resolved: Vec<PathBuf> = homes
            .iter()
            .filter(|h| h.is_absolute())
            .filter_map(|h| resolve_existing_prefix(h))
            .collect();
        resolved.dedup();
        Self {
            homes: resolved,
            user_home: user_home.filter(|u| u.is_absolute()),
        }
    }

    /// Inside a protected file or directory of a home. Compared in lower
    /// case: macOS file names ignore case.
    fn is_secret(&self, resolved: &Path) -> bool {
        let r = lower(resolved);
        self.homes.iter().any(|h| {
            let Ok(rel) = r.strip_prefix(lower(h)) else {
                return false;
            };
            let Some(Component::Normal(first)) = rel.components().next() else {
                return false;
            };
            let first = first.to_string_lossy();
            SECRET_FILES.contains(&first.as_ref())
                || first.starts_with(DATABASE)
                || SECRET_DIRS.contains(&first.as_ref())
        })
    }

    /// A home or a directory above it: a recursive read there reaches the secrets.
    fn holds_home(&self, resolved: &Path) -> bool {
        let r = lower(resolved);
        self.homes.iter().any(|h| lower(h).starts_with(&r))
    }

    /// `~`, `$HOME` and `$PROVEFAB_HOME` at the start of a word, as the shell
    /// would expand them.
    pub(super) fn expand(&self, word: &str) -> String {
        let with =
            |base: Option<&PathBuf>, rest: &str| base.map(|b| format!("{}{rest}", b.display()));
        let user = self.user_home.as_ref();
        let home = self.homes.first();
        let rules: [(&str, Option<&PathBuf>); 5] = [
            ("~", user),
            ("$HOME", user),
            ("${HOME}", user),
            ("$PROVEFAB_HOME", home),
            ("${PROVEFAB_HOME}", home),
        ];
        for (prefix, base) in rules {
            if let Some(rest) = word.strip_prefix(prefix)
                && (rest.is_empty() || rest.starts_with('/'))
                && let Some(expanded) = with(base, rest)
            {
                return expanded;
            }
        }
        word.to_string()
    }

    fn resolve(&self, word: &str, cwd: Option<&Path>) -> Option<PathBuf> {
        let p = Path::new(word);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd?.join(p)
        };
        Some(resolve_existing_prefix(&joined).unwrap_or(joined))
    }

    /// Why reading `word` is refused, if it is. `recursive`: the reader walks
    /// directories (a search tool, `grep -r`, `find`), so a home or a
    /// directory above one is refused too.
    pub(super) fn check_word(
        &self,
        word: &str,
        cwd: Option<&Path>,
        recursive: bool,
    ) -> Option<String> {
        let w = self.expand(word);
        let named = || {
            let l = w.to_lowercase();
            SECRET_NAMES.iter().any(|n| l.contains(n))
        };
        let refuse = || Some(format!("{word}: {WHY}"));
        if let Some(at) = w.find(EXPANSION) {
            if named() {
                return refuse();
            }
            // The directory the shell expands in: the home or above it can
            // match a secret.
            let literal = &w[..at];
            let dir = literal.rfind('/').map_or(".", |i| &literal[..=i]);
            let base = self.resolve(dir, cwd)?;
            return (self.is_secret(&base) || self.holds_home(&base))
                .then(refuse)
                .flatten();
        }
        match self.resolve(&w, cwd) {
            Some(r) => (self.is_secret(&r) || (recursive && self.holds_home(&r)))
                .then(refuse)
                .flatten(),
            // Relative, after a `cd` the guard could not follow.
            None => {
                let first_dir = Path::new(&w)
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().to_lowercase());
                (named() || first_dir.is_some_and(|d| SECRET_DIRS.contains(&d.as_str())))
                    .then(refuse)
                    .flatten()
            }
        }
    }
}

fn lower(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::super::{Decision, ToolCall, check_with};
    use super::*;

    /// A user home `u` whose Provefab home `u/.provefab` holds every secret
    /// kind, and a task worktree inside it with a link to the credentials.
    struct Fixture {
        _dir: tempfile::TempDir,
        user: PathBuf,
        home: PathBuf,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().canonicalize().unwrap();
        let home = user.join(".provefab");
        let root = home.join("worktrees/7");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
        for f in [
            "credentials.toml",
            "license.key",
            "provefab.db",
            "provefab.db-wal",
        ] {
            std::fs::write(home.join(f), "secret").unwrap();
        }
        for d in ["claude", "claude-api", "codex", "codex-api"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join("claude/.credentials.json"), "{}").unwrap();
        std::fs::write(home.join("codex-api/auth.json"), "{}").unwrap();
        std::os::unix::fs::symlink(home.join("credentials.toml"), root.join("leak")).unwrap();
        Fixture {
            _dir: dir,
            user,
            home,
            root,
        }
    }

    fn decide(f: &Fixture, call: &ToolCall) -> Decision {
        let protected = Protected::new(vec![f.home.clone()], Some(f.user.clone()));
        check_with(call, &f.root, &f.root, &protected)
    }

    fn shell(command: &str) -> ToolCall {
        ToolCall::Shell {
            command: command.into(),
        }
    }

    fn read(path: &Path) -> ToolCall {
        ToolCall::Read {
            paths: vec![path.to_path_buf()],
            pattern_is_option: false,
        }
    }

    #[test]
    fn read_tools_never_reach_the_secrets_in_the_home() {
        let f = fixture();
        let h = &f.home;
        for p in [
            h.join("credentials.toml"),
            h.join("license.key"),
            h.join("provefab.db"),
            h.join("provefab.db-wal"),
            h.join("claude/.credentials.json"),
            h.join("codex-api/auth.json"),
            h.join("claude-api"),
            h.join("Credentials.toml"),
            h.clone(),
            f.user.clone(),
            f.root.join("leak"),
            PathBuf::from("~/.provefab/credentials.toml"),
        ] {
            assert!(
                matches!(decide(&f, &read(&p)), Decision::Deny(_)),
                "{}",
                p.display()
            );
        }
        for p in [
            f.root.join("src/a.rs"),
            f.root.clone(),
            PathBuf::from("src"),
        ] {
            assert_eq!(decide(&f, &read(&p)), Decision::Allow, "{}", p.display());
        }
    }

    #[test]
    fn shell_commands_never_reach_the_secrets_in_the_home() {
        let f = fixture();
        let abs = f.home.join("credentials.toml").display().to_string();
        let denied = [
            "cat ~/.provefab/credentials.toml".to_string(),
            "cat $HOME/.provefab/credentials.toml".into(),
            "cat ${HOME}/.provefab/license.key".into(),
            format!("head -1 {abs}"),
            format!("cat < {abs}"),
            format!("cat <{abs}"),
            format!("grep --file={abs} x"),
            "grep -r token ~/.provefab".into(),
            "rg token ~".into(),
            "find ~/.provefab -name '*.toml'".into(),
            "cat leak".into(),
            "cat $X/credentials.toml".into(),
            "cat ~/.provefab/cred*".into(),
            "ls ~/.provefab/claude".into(),
            "cat ~/.provefab/codex-api/auth.json".into(),
            "cd ~/.provefab && cat credentials.toml".into(),
            "cd ~/.provefab && cat claude/.credentials.json".into(),
            format!("git diff --no-index {abs} /dev/null"),
            "echo ok; sqlite3 ~/.provefab/provefab.db .dump".into(),
            "security find-generic-password -s provefab-anthropic -w".into(),
            "/usr/bin/security dump-keychain".into(),
            "env security find-generic-password -s x".into(),
            "provefab secrets get anthropic".into(),
            "/opt/bin/provefab-pro secrets get anthropic".into(),
            "~/.provefab/bin/provefab secrets get anthropic".into(),
            "./renamed secrets get anthropic".into(),
            "xargs provefab secrets get".into(),
        ];
        for cmd in &denied {
            assert!(
                matches!(decide(&f, &shell(cmd)), Decision::Deny(_)),
                "allowed but must be denied: {cmd}"
            );
        }
        let unknown_cwd = ToolCall::ShellUnknownCwd {
            command: "cat ~/.provefab/credentials.toml".into(),
        };
        assert!(matches!(decide(&f, &unknown_cwd), Decision::Deny(_)));
        let inside = f.root.join("src/a.rs").display().to_string();
        for cmd in [
            "cat src/a.rs".to_string(),
            "grep -rn credentials.toml src".into(),
            "rg 'secrets get' src".into(),
            "grep -r x .".into(),
            format!("cat {inside}"),
            "cat ~/.provefab/worktrees/7/src/a.rs".into(),
            "ls".into(),
            "cargo test".into(),
            "echo 'provefab secrets get anthropic' > notes.txt".into(),
        ] {
            assert_eq!(decide(&f, &shell(&cmd)), Decision::Allow, "{cmd}");
        }
    }

    #[test]
    fn the_home_comes_from_the_environment_and_from_the_worktree() {
        let f = fixture();
        let p = Protected::from_env(&f.root);
        assert!(
            p.homes.contains(&f.home),
            "{:?} should hold {}",
            p.homes,
            f.home.display()
        );
    }
}
