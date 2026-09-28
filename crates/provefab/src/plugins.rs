//! The worker plugins ship inside the binary and are written to disk on first
//! use, so their version always matches the `provefab` that calls them (spec §5.4).

use std::path::{Path, PathBuf};

use include_dir::{Dir, DirEntry, include_dir};

static PLUGINS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../plugins");

/// Where the installed plugins live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugins {
    /// Pi package directory, passed to `pi -e`.
    pub pi_package: PathBuf,
    /// Claude Code plugin directory, passed to `claude --plugin-dir`.
    pub cc_plugin: PathBuf,
}

/// Writes both plugins under `base/<crate version>-<content hash>/` unless that
/// directory already exists. Shared skills are copied into each plugin.
pub fn install(base: &Path) -> std::io::Result<InstalledPlugins> {
    let root = base.join(format!(
        "{}-{:016x}",
        env!("CARGO_PKG_VERSION"),
        content_hash()
    ));
    let installed = InstalledPlugins {
        pi_package: root.join("pi-provefab"),
        cc_plugin: root.join("cc-provefab"),
    };
    if root.join(".complete").exists() {
        return Ok(installed);
    }
    let staging = base.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    for plugin in ["pi-provefab", "cc-provefab"] {
        let dir = PLUGINS
            .get_dir(plugin)
            .ok_or_else(|| std::io::Error::other(format!("{plugin} missing from the binary")))?;
        write_dir(dir, Path::new(plugin), &staging.join(plugin))?;
        if let Some(skills) = PLUGINS.get_dir("skills") {
            write_dir(
                skills,
                Path::new("skills"),
                &staging.join(plugin).join("skills"),
            )?;
        }
    }
    std::fs::write(staging.join(".complete"), "")?;
    // Another provefab process may have finished the same install first; either copy is identical.
    if std::fs::rename(&staging, &root).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    Ok(installed)
}

/// Writes `dir` (whose embedded path is `prefix`) into `dest`.
fn write_dir(dir: &Dir<'_>, prefix: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in dir.entries() {
        let rel = entry
            .path()
            .strip_prefix(prefix)
            .map_err(std::io::Error::other)?;
        match entry {
            DirEntry::Dir(d) => write_dir(d, d.path(), &dest.join(rel))?,
            DirEntry::File(f) => std::fs::write(dest.join(rel), f.contents())?,
        }
    }
    Ok(())
}

/// FNV-1a over every embedded path and file, so an edited plugin gets a new directory.
fn content_hash() -> u64 {
    fn visit(dir: &Dir<'_>, h: &mut u64) {
        for entry in dir.entries() {
            feed(h, entry.path().to_string_lossy().as_bytes());
            match entry {
                DirEntry::Dir(d) => visit(d, h),
                DirEntry::File(f) => feed(h, f.contents()),
            }
        }
    }
    fn feed(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h ^= u64::from(*b);
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325;
    visit(&PLUGINS, &mut h);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_both_plugins_with_shared_skills() {
        let base = tempfile::tempdir().unwrap();
        let p = install(base.path()).unwrap();
        assert!(p.pi_package.join("package.json").is_file());
        assert!(p.pi_package.join("extensions/provefab.ts").is_file());
        assert!(
            p.pi_package
                .join("skills/provefab-worker/SKILL.md")
                .is_file()
        );
        assert!(p.cc_plugin.join(".claude-plugin/plugin.json").is_file());
        assert!(
            p.cc_plugin
                .join("skills/rust-conventions/SKILL.md")
                .is_file()
        );
        let hooks = std::fs::read_to_string(p.cc_plugin.join("hooks/hooks.json")).unwrap();
        assert!(
            hooks.contains("guard --format claude-code || exit 2"),
            "{hooks}"
        );
        // Final review M10: a hung guard must not stall the agent for the 600 s default.
        let parsed: serde_json::Value = serde_json::from_str(&hooks).unwrap();
        assert_eq!(
            parsed["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 30,
            "{hooks}"
        );
    }

    #[test]
    fn install_is_idempotent_and_reuses_the_directory() {
        let base = tempfile::tempdir().unwrap();
        let a = install(base.path()).unwrap();
        std::fs::write(a.pi_package.join("marker"), "x").unwrap();
        let b = install(base.path()).unwrap();
        assert_eq!(a, b);
        assert!(
            b.pi_package.join("marker").exists(),
            "second install must not rewrite"
        );
        let entries: Vec<_> = std::fs::read_dir(base.path()).unwrap().collect();
        assert_eq!(entries.len(), 1, "no staging directory left behind");
    }

    #[test]
    fn plugin_json_files_are_valid() {
        let base = tempfile::tempdir().unwrap();
        let p = install(base.path()).unwrap();
        for f in [
            p.pi_package.join("package.json"),
            p.cc_plugin.join(".claude-plugin/plugin.json"),
            p.cc_plugin.join("hooks/hooks.json"),
        ] {
            let text = std::fs::read_to_string(&f).unwrap();
            serde_json::from_str::<serde_json::Value>(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        }
    }
}
