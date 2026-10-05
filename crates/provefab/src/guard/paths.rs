use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use super::Decision;

/// Relative to the worktree root. Agents may not touch provefab state, CI,
/// VCS internals, or the repo's own agent configuration.
const PROTECTED_DIRS: [&str; 5] = [".git", ".provefab", ".github/workflows", ".pi", ".claude"];
/// Device files that are always safe to write to.
const DEVICE_SINKS: [&str; 3] = ["/dev/null", "/dev/stdout", "/dev/stderr"];

pub(super) fn check_write(path: &Path, cwd: &Path, root: &Path) -> Decision {
    if DEVICE_SINKS.iter().any(|s| path == Path::new(s)) {
        return Decision::Allow;
    }
    let raw = path.to_string_lossy();
    if raw.starts_with('~') || raw.contains('$') {
        return deny(path, "paths using `~` or variables are not allowed");
    }
    if path.components().any(|c| c == Component::ParentDir) {
        // `..` after a symlink resolves differently in the OS than lexically; refuse it outright.
        return deny(path, "`..` is not allowed in paths");
    }
    let Ok(root) = root.canonicalize() else {
        return deny(path, "the worktree root does not exist");
    };
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let Some(resolved) = resolve_existing_prefix(&joined) else {
        return deny(path, "path cannot be resolved");
    };
    let Ok(rel) = resolved.strip_prefix(&root) else {
        return deny(path, "writes outside the worktree are not allowed");
    };
    // macOS filesystems are case-insensitive: `.Claude/` is `.claude/`.
    let rel = PathBuf::from(rel.to_string_lossy().to_lowercase());
    if let Some(dir) = PROTECTED_DIRS.iter().find(|d| rel.starts_with(d)) {
        return deny(path, &format!("`{dir}/` is protected"));
    }
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = rel
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.starts_with(".env") || ext == "pem" || ext == "key" {
        return deny(path, "secret files are protected");
    }
    Decision::Allow
}

/// A read in a review of a person's pull request (`PROVEFAB_UNTRUSTED_REVIEW`):
/// the path must resolve inside the worktree, symbolic links followed.
/// `~`, `$` and Pi's `@` prefix are refused rather than guessed at; `..` is
/// allowed only when the whole path exists, so the OS resolves it.
pub(super) fn check_read(path: &Path, cwd: &Path, root: &Path) -> Decision {
    let raw = path.to_string_lossy();
    if raw.starts_with('~') || raw.starts_with('@') || raw.contains('$') {
        return deny(path, "paths using `~`, `@` or variables are refused");
    }
    let Ok(root) = root.canonicalize() else {
        return deny(path, "the worktree root does not exist");
    };
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let resolved = if joined.components().any(|c| c == Component::ParentDir) {
        joined.canonicalize().ok()
    } else {
        resolve_existing_prefix(&joined)
    };
    let Some(resolved) = resolved else {
        return deny(path, "path cannot be resolved");
    };
    if resolved.starts_with(&root) {
        Decision::Allow
    } else {
        deny(
            path,
            "a pull request review reads nothing outside the worktree",
        )
    }
}

/// Canonicalises the longest existing ancestor (following symlinks), then
/// re-appends the components that do not exist yet.
pub(super) fn resolve_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        if let Ok(mut canon) = existing.canonicalize() {
            for part in missing.iter().rev() {
                canon.push(part);
            }
            return Some(canon);
        }
        // It exists but cannot be resolved: a dangling symlink or a loop. The
        // OS would follow it on write, so treat it as unresolvable.
        if std::fs::symlink_metadata(&existing).is_ok() {
            return None;
        }
        missing.push(existing.file_name()?.to_os_string());
        if !existing.pop() {
            return None;
        }
    }
}

fn deny(path: &Path, why: &str) -> Decision {
    Decision::Deny(format!("{}: {why}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), "hi").unwrap();
        std::os::unix::fs::symlink(std::env::temp_dir(), dir.path().join("escape")).unwrap();
        dir
    }

    #[test]
    fn write_table() {
        let wt = worktree();
        let root = wt.path();
        let abs_inside = root.join("src/new/deep/file.rs");
        let cases: [(&Path, bool); 20] = [
            (Path::new("src/lib.rs"), true),
            (Path::new("README.md"), true),
            (Path::new("src/new/deep/file.rs"), true),
            (&abs_inside, true),
            (Path::new(".github/ISSUE_TEMPLATE/bug.md"), true),
            (Path::new("/dev/null"), true),
            (Path::new("/etc/hosts"), false),
            (Path::new("../outside.txt"), false),
            (Path::new("src/../../outside.txt"), false),
            (Path::new("escape/owned.txt"), false),
            (Path::new("~/.bashrc"), false),
            (Path::new("$HOME/.bashrc"), false),
            (Path::new(".git/config"), false),
            (Path::new(".provefab/plan.md"), false),
            (Path::new(".github/workflows/ci.yml"), false),
            (Path::new(".claude/settings.json"), false),
            (Path::new(".pi/extensions/x.ts"), false),
            (Path::new(".env"), false),
            (Path::new("config/.env.local"), false),
            (Path::new("certs/server.pem"), false),
        ];
        for (path, allowed) in cases {
            let d = check_write(path, root, root);
            assert_eq!(d == Decision::Allow, allowed, "{}: {d:?}", path.display());
        }
    }

    /// Final review C1: a symlink to a file that does not exist yet must not be written through.
    #[test]
    fn review_c1_dangling_symlink_is_denied() {
        let wt = worktree();
        let root = wt.path();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("not-yet.txt"), root.join("dangle"))
            .unwrap();
        let d = check_write(Path::new("dangle"), root, root);
        assert!(matches!(d, Decision::Deny(_)), "{d:?}");
        assert!(!outside.path().join("not-yet.txt").exists());
    }

    /// Final review M2 (re-graded): macOS filesystems ignore case, so `.Claude/` is `.claude/`.
    #[test]
    fn review_m2_protected_names_ignore_case() {
        let wt = worktree();
        let root = wt.path();
        for p in [
            ".Claude/settings.json",
            ".PI/x.ts",
            ".GIT/config",
            "server.PEM",
            "id.Key",
            ".ENV",
        ] {
            let d = check_write(Path::new(p), root, root);
            assert!(matches!(d, Decision::Deny(_)), "{p}: {d:?}");
        }
    }

    #[test]
    fn missing_root_denies_everything() {
        let d = check_write(
            Path::new("a.txt"),
            Path::new("/nope"),
            Path::new("/nope/root"),
        );
        assert!(matches!(d, Decision::Deny(r) if r.contains("worktree root")));
    }
}
