//! Tool-call policy shared by every worker (spec §6). Workers call it through
//! `provefab guard`; this module only decides. It is a tripwire, not a sandbox:
//! when in doubt it denies.

pub mod adapters;
mod paths;
mod shell;

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Shell {
        command: String,
    },
    Write {
        path: PathBuf,
    },
    /// A shell command whose working directory the caller cannot know (Codex
    /// runs it in a `workdir` its hook payload omits): relative writes are refused.
    ShellUnknownCwd {
        command: String,
    },
    /// Codex `apply_patch`: every file the patch adds, updates, deletes or moves to.
    Patch {
        paths: Vec<PathBuf>,
    },
    /// Adapters map tools that must never run (network, MCP, malformed calls) here.
    Blocked {
        reason: String,
    },
    /// A read or search tool (Claude Code's Read, Grep, Glob; Pi's read,
    /// grep, find, ls) and the paths it reads, its default directory included.
    /// Allowed for every stage; a review of a person's pull request keeps it
    /// inside the worktree.
    Read {
        paths: Vec<PathBuf>,
        /// A search pattern starting with `-`, which Pi passes to `rg` or
        /// `fd` where an option goes.
        pattern_is_option: bool,
    },
    /// Read-only and other harmless tools.
    Other {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

/// `cwd` resolves relative paths; `root` is the task's worktree.
pub fn check(call: &ToolCall, cwd: &Path, root: &Path) -> Decision {
    match call {
        ToolCall::Shell { command } => shell::check_command(command, cwd, root),
        ToolCall::ShellUnknownCwd { command } => shell::check_command_from(command, None, root),
        ToolCall::Write { path } => paths::check_write(path, cwd, root),
        ToolCall::Patch { paths } if paths.is_empty() => {
            Decision::Deny("the patch names no file, so the guard cannot check it".into())
        }
        ToolCall::Patch { paths } => paths
            .iter()
            .map(|p| paths::check_write(p, cwd, root))
            .find(|d| *d != Decision::Allow)
            .unwrap_or(Decision::Allow),
        ToolCall::Blocked { reason } => Decision::Deny(reason.clone()),
        ToolCall::Read { .. } | ToolCall::Other { .. } => Decision::Allow,
    }
}

/// `check` for a review of a person's pull request (`PROVEFAB_UNTRUSTED_REVIEW`,
/// final review I1): no file is written, the shell runs only read-only
/// programs, so nothing the pull request wrote runs during its review, and
/// every read stays inside the worktree.
pub fn check_review(call: &ToolCall, cwd: &Path, root: &Path) -> Decision {
    match call {
        ToolCall::Shell { command } => {
            shell::check_review_command(command, Some(cwd.to_path_buf()), root)
        }
        ToolCall::ShellUnknownCwd { command } => shell::check_review_command(command, None, root),
        ToolCall::Read {
            pattern_is_option: true,
            ..
        } => Decision::Deny(
            "a search pattern starting with `-` is refused in a pull request review".into(),
        ),
        ToolCall::Read { paths, .. } => paths
            .iter()
            .map(|p| paths::check_read(p, cwd, root))
            .find(|d| *d != Decision::Allow)
            .unwrap_or(Decision::Allow),
        ToolCall::Write { .. } | ToolCall::Patch { .. } => {
            Decision::Deny("a pull request review writes no file".into())
        }
        ToolCall::Blocked { reason } => Decision::Deny(reason.clone()),
        ToolCall::Other { .. } => Decision::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_is_allowed_only_if_every_path_is() {
        let wt = tempfile::tempdir().unwrap();
        let root = wt.path();
        let patch = |paths: &[&str]| ToolCall::Patch {
            paths: paths.iter().map(PathBuf::from).collect(),
        };
        assert_eq!(
            check(&patch(&["src/a.rs", "README.md"]), root, root),
            Decision::Allow
        );
        assert!(matches!(
            check(&patch(&["src/a.rs", "/etc/hosts"]), root, root),
            Decision::Deny(_)
        ));
        assert!(matches!(
            check(&patch(&[".github/workflows/ci.yml"]), root, root),
            Decision::Deny(_)
        ));
        assert!(matches!(check(&patch(&[]), root, root), Decision::Deny(_)));
    }
}
