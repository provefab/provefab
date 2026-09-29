//! Where Provefab keeps its state: `~/.provefab` unless `PROVEFAB_HOME` is set.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    /// `PROVEFAB_HOME`, else `$HOME/.provefab`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("PROVEFAB_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".provefab")))
            .unwrap_or_else(|| PathBuf::from(".provefab"));
        Self { home }
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
