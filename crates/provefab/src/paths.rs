//! Where Provefab keeps its state: `~/.provefab` unless `PROVEFAB_HOME` is set.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `dir` created 0700, its missing parents with the default mode; nothing
/// changes when `dir` already exists.
pub fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        // Created meanwhile by another process: as good.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `PROVEFAB_HOME`, else `$HOME/.provefab`, made absolute.
    pub fn from_env() -> Self {
        Self::from_vars(std::env::var_os("PROVEFAB_HOME"), std::env::var_os("HOME"))
    }

    /// `from_env` with the two variables given. The home is made absolute
    /// against the current directory once, here: the key helper, the
    /// systemd unit and the launchd agent keep it, and a relative path there
    /// would point elsewhere (Linux final review).
    pub fn from_vars(provefab_home: Option<OsString>, home: Option<OsString>) -> Self {
        let home = provefab_home
            .map(PathBuf::from)
            .or_else(|| home.map(|h| PathBuf::from(h).join(".provefab")))
            .unwrap_or_else(|| PathBuf::from(".provefab"));
        let home = std::path::absolute(&home).unwrap_or(home);
        Self { home }
    }

    /// Creates the home for its owner only (0700) when it does not exist yet;
    /// an existing directory keeps its mode (Linux final review, C1).
    pub fn ensure_home(&self) -> std::io::Result<()> {
        create_private_dir_all(&self.home)
    }

    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    pub fn config(&self) -> PathBuf {
        self.home.join("provefab.toml")
    }

    pub fn db(&self) -> PathBuf {
        self.home.join("provefab.db")
    }

    /// Cached model prices (D71).
    pub fn prices(&self) -> PathBuf {
        self.home.join("prices.json")
    }

    pub fn plugins(&self) -> PathBuf {
        self.home.join("plugins")
    }

    /// `CLAUDE_CONFIG_DIR` for the Claude Code worker (spec §5.3).
    pub fn claude_config(&self) -> PathBuf {
        self.home.join("claude")
    }

    /// `CODEX_HOME` for the Codex worker (spec §5.5).
    pub fn codex_home(&self) -> PathBuf {
        self.home.join("codex")
    }

    /// `CLAUDE_CONFIG_DIR` for Claude Code models signed in by API key.
    pub fn claude_config_api(&self) -> PathBuf {
        self.home.join("claude-api")
    }

    /// `CODEX_HOME` for Codex models signed in by API key.
    pub fn codex_home_api(&self) -> PathBuf {
        self.home.join("codex-api")
    }

    /// The task's git worktree. Outside the user's checkout, one per task.
    pub fn worktree(&self, task_id: i64) -> PathBuf {
        self.home.join("worktrees").join(task_id.to_string())
    }

    /// Transcripts and scratch files for one stage attempt. Never inside the
    /// worktree, so nothing here can end up in a commit.
    pub fn session(&self, task_id: i64, stage: &str, attempt: u32) -> PathBuf {
        self.home
            .join("sessions")
            .join(task_id.to_string())
            .join(format!("{stage}-{attempt}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux final review: a relative `PROVEFAB_HOME` becomes absolute once,
    /// so the key helper and the service file never hold a relative path.
    #[test]
    fn the_home_is_always_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let rel = Paths::from_vars(Some("rel/home".into()), None);
        assert_eq!(rel.home, cwd.join("rel/home"));
        let nothing = Paths::from_vars(None, None);
        assert_eq!(nothing.home, cwd.join(".provefab"));
        let abs = Paths::from_vars(Some("/srv/pf".into()), Some("/home/a".into()));
        assert_eq!(abs.home, PathBuf::from("/srv/pf"));
        let user = Paths::from_vars(None, Some("/home/a".into()));
        assert_eq!(user.home, PathBuf::from("/home/a/.provefab"));
    }

    /// Linux final review (C1): a home Provefab creates is its owner's only;
    /// an existing one keeps its mode.
    #[test]
    fn a_new_home_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fresh = Paths::new(&dir.path().join("a/.provefab"));
        fresh.ensure_home().unwrap();
        assert_eq!(mode(&fresh.home), 0o700);
        // Only the home itself: a parent it creates gets the default mode.
        let reference = dir.path().join("reference");
        std::fs::create_dir(&reference).unwrap();
        assert_eq!(mode(&dir.path().join("a")), mode(&reference));
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        Paths::new(&open).ensure_home().unwrap();
        assert_eq!(mode(&open), 0o755);
    }

    #[test]
    fn layout_keeps_sessions_outside_worktrees() {
        let p = Paths::new(Path::new("/h/.provefab"));
        assert_eq!(p.config(), PathBuf::from("/h/.provefab/provefab.toml"));
        assert_eq!(p.worktree(7), PathBuf::from("/h/.provefab/worktrees/7"));
        assert_eq!(
            p.session(7, "implement", 2),
            PathBuf::from("/h/.provefab/sessions/7/implement-2")
        );
        assert!(!p.session(7, "plan", 0).starts_with(p.worktree(7)));
        assert_eq!(p.codex_home(), PathBuf::from("/h/.provefab/codex"));
    }
}
