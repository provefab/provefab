//! What an agent runs to configure Provefab (agent setup spec): `init`,
//! `repos add`, the JSON lines of `doctor --json`, and their exit codes.

use std::ffi::OsStr;
use std::io::Write as _;

use crate::config::Config;
use crate::paths::Paths;

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
}
