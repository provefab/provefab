//! Repository declarations, with no automatic claim about validation dependency closure.
use crate::{
    config::{RepoConfig, SandboxConfig},
    paths::Paths,
};
use agent_workers::SandboxPolicy;
use std::io;
use std::path::{Component, Path, PathBuf};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn literal(path: &Path, absolute: bool) -> io::Result<()> {
    if (absolute && !path.is_absolute())
        || path.as_os_str().is_empty()
        || path.components().any(|c| c == Component::ParentDir)
        || path
            .to_str()
            .is_none_or(|p| p.contains(['*', '?', '[', ']', '\0']))
    {
        return Err(invalid(format!(
            "expected a literal {}path without '..': {}",
            if absolute { "absolute " } else { "" },
            path.display()
        )));
    }
    Ok(())
}
pub fn validate_config(config: &SandboxConfig) -> io::Result<()> {
    for path in &config.protected_paths {
        literal(path, false)?;
    }
    for path in &config.deny_read {
        literal(path, true)?;
    }
    if config.runtime.as_os_str().is_empty() {
        return Err(invalid("runtime must name an executable"));
    }
    Ok(())
}
/// Resolve absent secret leaves without dropping declarations. Broken symlinks are errors.
pub(crate) fn resolve(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path).is_err() =>
        {
            Ok(resolve(path.parent().ok_or(error)?)?.join(
                path.file_name()
                    .ok_or_else(|| invalid("path has no name"))?,
            ))
        }
        Err(error) => Err(error),
    }
}
/// Derive against the actual validation repository, including NoTools' separate empty cwd.
pub fn derive(
    repo: &RepoConfig,
    paths: &Paths,
    validation_root: &Path,
    scratch: &Path,
) -> io::Result<SandboxPolicy> {
    validate_config(&repo.sandbox)?;
    let root = validation_root.canonicalize()?;
    let temporary = resolve(&std::path::absolute(scratch)?.join("sandbox-tmp"))?;
    let mut policy = SandboxPolicy {
        runtime: repo.sandbox.runtime.clone(),
        ..Default::default()
    };
    for declared in &repo.sandbox.protected_paths {
        let path = if declared.is_absolute() {
            declared.clone()
        } else {
            root.join(declared)
        };
        literal(&path, true)?;
        // A mutable alias can be replaced after preparation even when its current target is safe.
        let mut prefix = PathBuf::new();
        for component in path.components() {
            prefix.push(component);
            let metadata = std::fs::symlink_metadata(&prefix)?;
            if prefix.starts_with(&root) && metadata.file_type().is_symlink() {
                return Err(invalid(format!(
                    "protected path uses a worktree symlink: {}",
                    prefix.display()
                )));
            }
        }
        let target = if path.starts_with(&root) {
            let target = path.canonicalize()?;
            if !target.starts_with(&root) {
                return Err(invalid("protected worktree path escapes its root"));
            }
            target
        } else {
            agent_workers::trusted_path_target(&path, &root, &temporary)
                .ok_or_else(|| invalid(format!("external protected path is missing, ambiguous or traverses a writable root: {}; declare a stable external path", path.display())))?
        };
        if root.starts_with(&target) {
            return Err(invalid(
                "protected path must not replace or encompass the worktree root",
            ));
        }
        policy.deny_write.extend([path, target]);
    }
    policy
        .deny_read
        .extend(repo.sandbox.deny_read.iter().cloned());
    // Paths.home is an explicit dependency, not inferred from the daemon's HOME.
    let mut homes = vec![std::path::absolute(&paths.home)?, Paths::from_env().home];
    if let Some(home) = std::env::var_os("HOME") {
        homes.push(PathBuf::from(home).join(".provefab"));
    }
    for home in homes {
        for name in [
            "credentials.toml",
            "license.key",
            "provefab.db",
            "provefab.db-wal",
            "provefab.db-shm",
            "provefab.db-journal",
            "claude",
            "claude-api",
            "codex",
            "codex-api",
        ] {
            policy.deny_read.push(home.join(name));
        }
        policy.deny_write.push(home.join("provefab.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        // File-denial evidence for a disposable keychain does not establish API isolation.
        policy
            .deny_read
            .push(PathBuf::from(home).join("Library/Keychains"));
    }
    for key in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "PI_CODING_AGENT_DIR"] {
        if let Some(path) = std::env::var_os(key) {
            let path = PathBuf::from(path);
            literal(&path, true)?;
            policy.deny_read.push(path);
        }
    }
    for list in [&mut policy.deny_read, &mut policy.deny_write] {
        let originals = list.clone();
        for path in originals {
            literal(&path, true)?;
            list.push(resolve(&path)?);
        }
        list.sort();
        list.dedup();
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repository(root: &Path) -> RepoConfig {
        let mut repo = crate::config::Config::from_toml_str(
            "models=[]\n[jev]\nmodel='jev-1.13.0'\n[[repos]]\nslug='o/r'\ngates=['true']\n",
        )
        .unwrap()
        .repos
        .remove(0);
        repo.local_path = Some(root.into());
        repo
    }
    #[test]
    fn derivation_preserves_protected_files_and_explicit_home_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        let home = dir.path().join("explicit-home");
        std::fs::create_dir_all(root.join("checks")).unwrap();
        std::fs::write(root.join("checks/check.sh"), "validator").unwrap();
        let mut repo = repository(&root);
        repo.sandbox.protected_paths = vec!["checks/check.sh".into()];
        let policy = derive(
            &repo,
            &Paths::new(&home),
            &root,
            &dir.path().join("session"),
        )
        .unwrap();
        assert!(
            policy
                .deny_write
                .contains(&root.join("checks/check.sh").canonicalize().unwrap())
        );
        assert!(
            policy
                .deny_read
                .contains(&resolve(&home.join("provefab.db-wal")).unwrap())
        );
        assert!(
            policy
                .deny_read
                .contains(&resolve(&home.join("claude-api")).unwrap())
        );
        assert!(
            !policy.allow_local_network,
            "workers never inherit gate network opt-in"
        );
        for declaration in ["missing", ".", "checks/link"] {
            if declaration == "checks/link" {
                std::os::unix::fs::symlink(root.join("checks/check.sh"), root.join("checks/link"))
                    .unwrap();
            }
            repo.sandbox.protected_paths = vec![declaration.into()];
            assert!(
                derive(
                    &repo,
                    &Paths::new(&home),
                    &root,
                    &dir.path().join("session")
                )
                .is_err(),
                "{declaration}"
            );
        }
    }

    #[test]
    fn external_validation_alias_must_not_traverse_mutable_worktree_components() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        let outside = dir.path().join("outside");
        let safe = dir.path().join("safe");
        for path in [&root, &outside, &safe] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(safe.join("check.sh"), "safe").unwrap();
        std::os::unix::fs::symlink(&safe, root.join("alias")).unwrap();
        std::os::unix::fs::symlink(root.join("alias"), outside.join("redirect")).unwrap();
        let mut repo = repository(&root);
        repo.sandbox.protected_paths = vec![outside.join("redirect/check.sh")];
        assert!(
            derive(
                &repo,
                &Paths::new(&dir.path().join("home")),
                &root,
                &dir.path().join("session")
            )
            .is_err(),
            "accepted mutable intermediate alias"
        );
        // A symlink target containing writable components then '..' is equally replaceable.
        std::os::unix::fs::symlink(root.join("alias/../../safe"), outside.join("dotdot")).unwrap();
        repo.sandbox.protected_paths = vec![outside.join("dotdot/check.sh")];
        assert!(
            derive(
                &repo,
                &Paths::new(&dir.path().join("home")),
                &root,
                &dir.path().join("session")
            )
            .is_err()
        );
        let scratch = dir.path().join("session");
        std::fs::create_dir_all(scratch.join("sandbox-tmp")).unwrap();
        std::os::unix::fs::symlink(&safe, scratch.join("sandbox-tmp/alias")).unwrap();
        std::os::unix::fs::symlink(scratch.join("sandbox-tmp/alias"), outside.join("tmp-alias"))
            .unwrap();
        repo.sandbox.protected_paths = vec![outside.join("tmp-alias/check.sh")];
        assert!(
            derive(
                &repo,
                &Paths::new(&dir.path().join("home")),
                &root,
                &scratch
            )
            .is_err()
        );
    }
}
