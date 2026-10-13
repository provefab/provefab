//! Immutable native policy for direct file tools. Shell descendants are enforced by the OS.
use super::{Decision, ToolCall, paths::resolve_existing_prefix};
use serde::Deserialize;
use std::{
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Policy {
    filesystem: Filesystem,
    network: Network,
    enable_weaker_nested_sandbox: bool,
    enable_weaker_network_isolation: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Network {
    allowed_domains: Vec<String>,
    denied_domains: Vec<String>,
    strict_allowed_domains: bool,
    allow_local_binding: bool,
    allow_all_unix_sockets: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Filesystem {
    deny_read: Vec<PathBuf>,
    deny_write: Vec<PathBuf>,
    allow_write: Vec<PathBuf>,
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "native sandbox policy missing, malformed or mutable",
    )
}
fn literal(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| c == Component::ParentDir)
        && path
            .to_str()
            .is_some_and(|s| !s.contains(['*', '?', '[', ']', '$', '\0']))
}
fn load(path: &Path, root: &Path) -> io::Result<Filesystem> {
    let root = root.canonicalize()?;
    if !literal(path) || std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid());
    }
    let canonical = agent_workers::trusted_path_target(path, &root, &root).ok_or_else(invalid)?;
    if canonical.starts_with(&root) {
        return Err(invalid());
    }
    // Bound allocation and reject special files before reading the trusted policy.
    let metadata = std::fs::metadata(&canonical)?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
        return Err(invalid());
    }
    let policy: Policy =
        serde_json::from_slice(&std::fs::read(&canonical)?).map_err(|_| invalid())?;
    if policy.enable_weaker_nested_sandbox
        || policy.enable_weaker_network_isolation
        || !policy.network.allowed_domains.is_empty()
        || policy.network.denied_domains != ["*"]
        || !policy.network.strict_allowed_domains
        || policy.network.allow_local_binding
        || policy.network.allow_all_unix_sockets
    {
        return Err(invalid());
    }
    let mut fs = policy.filesystem;
    if fs.allow_write.is_empty()
        || fs
            .deny_read
            .iter()
            .chain(&fs.deny_write)
            .chain(&fs.allow_write)
            .any(|p| !literal(p))
        || fs.allow_write.iter().any(|p| canonical.starts_with(p))
    {
        return Err(invalid());
    }
    for paths in [&mut fs.deny_read, &mut fs.deny_write, &mut fs.allow_write] {
        let originals = paths.clone();
        for path in originals {
            paths.push(resolve_existing_prefix(&path).ok_or_else(invalid)?);
        }
        paths.sort();
        paths.dedup();
    }
    if fs.allow_write.iter().any(|p| canonical.starts_with(p)) {
        return Err(invalid());
    }
    Ok(fs)
}
fn intersect(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn candidates(path: &Path, cwd: &Path) -> io::Result<[PathBuf; 2]> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    if !literal(&joined) || path.to_string_lossy().starts_with(['~', '@']) {
        return Err(invalid());
    }
    let target = resolve_existing_prefix(&joined).ok_or_else(invalid)?;
    Ok([joined, target])
}
/// Searches may follow nested links. Inspect real directory entries with a bounded budget;
/// a broken link or unreadable subtree refuses instead of silently omitting a possible secret.
fn check_descendants(path: &Path, denied: &[PathBuf]) -> io::Result<()> {
    let mut pending = vec![path.to_path_buf()];
    let mut visited = std::collections::HashSet::new();
    let mut remaining = 100_000usize;
    while let Some(path) = pending.pop() {
        remaining = remaining.checked_sub(1).ok_or_else(invalid)?;
        let target = resolve_existing_prefix(&path).ok_or_else(invalid)?;
        if denied.iter().any(|p| intersect(&target, p)) {
            return Err(invalid());
        }
        if target.is_dir() && visited.insert(target.clone()) {
            for entry in std::fs::read_dir(target)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}
fn check_path(
    path: &Path,
    cwd: &Path,
    root: &Path,
    fs: &Filesystem,
    write: bool,
) -> io::Result<()> {
    let candidates = candidates(path, cwd)?;
    let denied = if write { &fs.deny_write } else { &fs.deny_read };
    for candidate in &candidates {
        if denied.iter().any(|p| intersect(candidate, p)) {
            return Err(invalid());
        }
    }
    if !write && candidates[1].is_dir() {
        check_descendants(&candidates[1], denied)?;
    }
    if write {
        let root = root.canonicalize()?;
        // Replacing the root replaces protected descendants too, including native policy boundaries.
        if candidates
            .iter()
            .any(|p| p == &root || !p.starts_with(&root))
            || !fs.allow_write.iter().any(|p| root.starts_with(p))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
/// No env means the historical standalone guard. A named invalid policy always denies.
pub(super) fn check(call: &ToolCall, cwd: &Path, root: &Path) -> Decision {
    let Some(path) = std::env::var_os(agent_workers::SANDBOX_POLICY_ENV) else {
        return Decision::Allow;
    };
    let checked = (|| {
        let fs = load(Path::new(&path), root)?;
        match call {
            ToolCall::Write { path } => check_path(path, cwd, root, &fs, true)?,
            ToolCall::Patch { paths } => {
                for path in paths {
                    check_path(path, cwd, root, &fs, true)?;
                }
            }
            ToolCall::Read { paths, .. } => {
                if paths.is_empty() {
                    return Err(invalid());
                }
                for path in paths {
                    check_path(path, cwd, root, &fs, false)?;
                }
            }
            _ => {}
        }
        Ok::<_, io::Error>(())
    })();
    match checked {
        Ok(()) => Decision::Allow,
        Err(_) => Decision::Deny(
            "native sandbox policy refuses this tool path or cannot be verified".into(),
        ),
    }
}
/// Structured answers are the only NoTools exception, but named policy errors still refuse.
pub(crate) fn validate_named(root: &Path) -> Decision {
    check(
        &ToolCall::Other {
            name: "answer".into(),
        },
        root,
        root,
    )
}
