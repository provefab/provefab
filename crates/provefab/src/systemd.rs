//! The systemd user service (Linux design spec §4):
//! `~/.config/systemd/user/provefab.service`, managed with `systemctl --user`.
//! Lingering is read and reported, never turned on (spec decision 6).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::commands::Check;
use crate::service::ServiceError;

/// The unit's file name in the user unit directory.
pub const UNIT: &str = "provefab.service";

/// `%` is a specifier in every unit value.
fn plain(s: &str) -> String {
    s.replace('%', "%%")
}

/// A double-quoted value: `\` and `"` escaped, `%` doubled.
fn quoted(s: &str) -> String {
    format!(
        "\"{}\"",
        plain(&s.replace('\\', "\\\\").replace('"', "\\\""))
    )
}

/// The unit (plan decisions 14, 15): the stable binary, `run --workers N`,
/// the frozen `PATH`, Provefab home, both streams appended to
/// `<home>/logs/run.log`. `$` is doubled in `ExecStart`, where systemd
/// expands variables; `Environment=` does not expand them.
pub fn unit(bin: &Path, workers: usize, path_env: &str, provefab_home: &Path) -> String {
    let home = provefab_home.display().to_string();
    let log = provefab_home
        .join("logs")
        .join("run.log")
        .display()
        .to_string();
    format!(
        "[Unit]
Description=Provefab: labelled issues to tested pull requests

[Service]
ExecStart={exec} run --workers {workers}
WorkingDirectory={dir}
Environment={path}
Environment={pf_home}
Restart=always
RestartSec=30
KillMode=mixed
TimeoutStopSec=30
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
",
        exec = quoted(&bin.display().to_string()).replace('$', "$$"),
        dir = plain(&home),
        path = quoted(&format!("PATH={path_env}")),
        pf_home = quoted(&format!("PROVEFAB_HOME={home}")),
        log = plain(&log),
    )
}

/// `$USER` when it is a plain account name, else the numeric uid; both are
/// accepted by `loginctl` (plan decision 16).
pub fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| {
            !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
        // SAFETY: getuid has no preconditions and cannot fail.
        .unwrap_or_else(|| unsafe { libc::getuid() }.to_string())
}

/// The systemd backend; tests point it at temporary directories and fake
/// `systemctl` and `loginctl` programs.
pub struct Systemd {
    pub systemctl: PathBuf,
    pub loginctl: PathBuf,
    /// `~/.config/systemd/user`, or `$XDG_CONFIG_HOME/systemd/user`.
    pub unit_dir: PathBuf,
    pub provefab_home: PathBuf,
    pub user: String,
}

impl Systemd {
    pub fn for_user(provefab_home: &Path) -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        Self {
            systemctl: "systemctl".into(),
            loginctl: "loginctl".into(),
            unit_dir: config.join("systemd").join("user"),
            provefab_home: provefab_home.to_path_buf(),
            user: current_user(),
        }
    }

    pub fn unit_path(&self) -> PathBuf {
        self.unit_dir.join(UNIT)
    }

    /// The copy systemd runs: rebuilding the checkout never pulls it away (D47).
    pub fn binary(&self) -> PathBuf {
        crate::service::binary_path(&self.provefab_home)
    }

    async fn systemctl(&self, args: &[&str]) -> Result<String, ServiceError> {
        let mut all = vec!["--user"];
        all.extend_from_slice(args);
        let shown = all.join(" ");
        let out = Command::new(&self.systemctl)
            .args(&all)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| ServiceError::Systemctl {
                args: shown.clone(),
                message: e.to_string(),
            })?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(text)
        } else {
            Err(ServiceError::Systemctl {
                args: shown,
                message: text.trim().to_string(),
            })
        }
    }

    /// Copies `exe`, writes the unit, reloads, enables and restarts it
    /// (plan decision 13).
    pub async fn install(
        &self,
        exe: &Path,
        workers: usize,
        path_env: &str,
    ) -> Result<PathBuf, ServiceError> {
        if path_env.trim().is_empty() {
            return Err(ServiceError::EmptyPath);
        }
        let texts = [
            exe.display().to_string(),
            self.provefab_home.display().to_string(),
            path_env.to_string(),
        ];
        if texts.iter().any(|t| t.contains('\n') || t.contains('\r')) {
            return Err(ServiceError::LineBreak);
        }
        let io = |what: &Path, e: std::io::Error| {
            ServiceError::Io(what.display().to_string(), e.to_string())
        };
        std::fs::create_dir_all(&self.unit_dir).map_err(|e| io(&self.unit_dir, e))?;
        let bin = crate::service::prepare(&self.provefab_home, exe)?;
        let path = self.unit_path();
        std::fs::write(&path, unit(&bin, workers, path_env, &self.provefab_home))
            .map_err(|e| io(&path, e))?;
        self.systemctl(&["daemon-reload"]).await?;
        self.systemctl(&["enable", UNIT]).await?;
        self.systemctl(&["restart", UNIT]).await?;
        Ok(path)
    }

    pub async fn uninstall(&self) -> Result<(), ServiceError> {
        let _ = self.systemctl(&["disable", "--now", UNIT]).await;
        let p = self.unit_path();
        if p.exists() {
            std::fs::remove_file(&p)
                .map_err(|e| ServiceError::Io(p.display().to_string(), e.to_string()))?;
        }
        let _ = self.systemctl(&["daemon-reload"]).await;
        Ok(())
    }

    /// `running (pid N)`, `loaded, <state>`, `installed, not loaded`, or
    /// `not installed`, as on launchd.
    pub async fn status(&self) -> String {
        let shown = self
            .systemctl(&["show", UNIT, "--property=LoadState,ActiveState,MainPID"])
            .await;
        let field = |text: &str, key: &str| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(key))
                .map(|v| v.trim().to_string())
        };
        match shown {
            Ok(text) if field(&text, "LoadState=").as_deref() == Some("loaded") => {
                let active = field(&text, "ActiveState=").unwrap_or_default();
                match field(&text, "MainPID=").filter(|p| p != "0") {
                    Some(pid) if active == "active" => format!("running (pid {pid})"),
                    _ => format!("loaded, {active}"),
                }
            }
            _ if self.unit_path().exists() => "installed, not loaded".into(),
            _ => "not installed".into(),
        }
    }

    /// `Some(true)` when the user lingers, `None` when `loginctl` did not
    /// say (it fails for a user with no session and no lingering).
    pub async fn linger(&self) -> Option<bool> {
        let out = Command::new(&self.loginctl)
            .args(["show-user", &self.user, "--property=Linger"])
            .stdin(Stdio::null())
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&out.stdout).trim() {
            "Linger=yes" => Some(true),
            "Linger=no" => Some(false),
            _ => None,
        }
    }

    fn enable_linger(&self) -> String {
        format!("sudo loginctl enable-linger {}", self.user)
    }

    /// Spec §4: when lingering is off, the command to run; never run it.
    pub async fn notes(&self) -> Vec<String> {
        match self.linger().await {
            Some(true) => Vec::new(),
            Some(false) => vec![format!(
                "lingering is off for {}: the service stops when you log out and does not start at boot; to change that, run: {}",
                self.user,
                self.enable_linger()
            )],
            None => vec![format!(
                "could not read lingering for {} with loginctl; if the service must run while you are logged out, run: {}",
                self.user,
                self.enable_linger()
            )],
        }
    }

    /// `doctor`'s `lingering` line, when the unit is installed.
    pub async fn checks(&self) -> Vec<Check> {
        if !self.unit_path().exists() {
            return Vec::new();
        }
        let (ok, detail) = match self.linger().await {
            Some(true) => (true, "on: the service runs without a login session"),
            Some(false) => (
                false,
                "off: the service stops at logout and does not start at boot",
            ),
            None => (false, "unknown: loginctl did not answer"),
        };
        vec![Check {
            name: "lingering".into(),
            ok,
            detail: detail.into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake(dir: &Path, name: &str, script: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// `systemctl` logging its arguments; `show` prints `show` from the
    /// file `show.txt` when it exists, else fails like a missing user bus.
    fn systemd(dir: &Path, linger: &str) -> Systemd {
        let calls = dir.join("calls.txt");
        let show = dir.join("show.txt");
        Systemd {
            systemctl: fake(
                dir,
                "systemctl",
                &format!(
                    "echo \"$@\" >> {calls}\nif [ \"$2\" = show ]; then cat {show} || {{ echo 'Failed to connect to bus' >&2; exit 1; }}; fi",
                    calls = calls.display(),
                    show = show.display()
                ),
            ),
            loginctl: fake(dir, "loginctl", linger),
            unit_dir: dir.join("config/systemd/user"),
            provefab_home: dir.join("home"),
            user: "alice".into(),
        }
    }

    fn calls(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls.txt")).unwrap_or_default()
    }

    #[test]
    fn the_unit_runs_the_stable_binary_with_path_home_and_logs() {
        let u = unit(
            Path::new("/home/a/.provefab/bin/provefab"),
            2,
            "/usr/local/bin:/usr/bin",
            Path::new("/home/a/.provefab"),
        );
        for expected in [
            "ExecStart=\"/home/a/.provefab/bin/provefab\" run --workers 2\n",
            "WorkingDirectory=/home/a/.provefab\n",
            "Environment=\"PATH=/usr/local/bin:/usr/bin\"\n",
            "Environment=\"PROVEFAB_HOME=/home/a/.provefab\"\n",
            "Restart=always\n",
            "RestartSec=30\n",
            "KillMode=mixed\n",
            "TimeoutStopSec=30\n",
            "StandardOutput=append:/home/a/.provefab/logs/run.log\n",
            "StandardError=append:/home/a/.provefab/logs/run.log\n",
            "[Install]\nWantedBy=default.target\n",
        ] {
            assert!(u.contains(expected), "missing {expected:?} in\n{u}");
        }
    }

    #[test]
    fn unit_values_are_escaped() {
        let u = unit(
            Path::new("/h/a b%/bin/pro\"fab"),
            1,
            "/p$x:/q%y:/r\\s",
            Path::new("/h/a b%"),
        );
        assert!(
            u.contains("ExecStart=\"/h/a b%%/bin/pro\\\"fab\" run --workers 1\n"),
            "{u}"
        );
        assert!(
            u.contains("Environment=\"PATH=/p$x:/q%%y:/r\\\\s\"\n"),
            "{u}"
        );
        assert!(
            u.contains("Environment=\"PROVEFAB_HOME=/h/a b%%\"\n"),
            "{u}"
        );
        assert!(u.contains("WorkingDirectory=/h/a b%%\n"), "{u}");
        assert!(
            u.contains("StandardOutput=append:/h/a b%%/logs/run.log\n"),
            "{u}"
        );
        let dollar = unit(
            Path::new("/h/$HOME/provefab"),
            1,
            "/usr/bin",
            Path::new("/h"),
        );
        assert!(
            dollar.contains("ExecStart=\"/h/$$HOME/provefab\""),
            "{dollar}"
        );
    }

    #[tokio::test]
    async fn install_reloads_enables_and_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(dir.path(), "echo Linger=yes");
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        let path = s.install(&exe, 3, "/usr/bin").await.unwrap();
        assert_eq!(path, s.unit_path());
        assert_eq!(
            path,
            dir.path().join("config/systemd/user/provefab.service")
        );
        assert_eq!(std::fs::read_to_string(s.binary()).unwrap(), "binary");
        assert_eq!(
            std::fs::metadata(s.binary()).unwrap().permissions().mode() & 0o111,
            0o111
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("run --workers 3")
        );
        assert!(dir.path().join("home/logs").is_dir());
        assert_eq!(
            calls(dir.path()),
            "--user daemon-reload\n--user enable provefab.service\n--user restart provefab.service\n"
        );
        std::fs::write(
            dir.path().join("show.txt"),
            "LoadState=loaded\nActiveState=active\nMainPID=42\n",
        )
        .unwrap();
        assert_eq!(s.status().await, "running (pid 42)");
        s.uninstall().await.unwrap();
        assert!(!path.exists());
        assert!(
            calls(dir.path())
                .ends_with("--user disable --now provefab.service\n--user daemon-reload\n")
        );
    }

    #[tokio::test]
    async fn status_reads_systemctl_show() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(dir.path(), "echo Linger=yes");
        // No user bus and no unit.
        assert_eq!(s.status().await, "not installed");
        std::fs::create_dir_all(&s.unit_dir).unwrap();
        std::fs::write(s.unit_path(), "unit").unwrap();
        assert_eq!(s.status().await, "installed, not loaded");
        let show = dir.path().join("show.txt");
        std::fs::write(
            &show,
            "LoadState=not-found\nActiveState=inactive\nMainPID=0\n",
        )
        .unwrap();
        assert_eq!(s.status().await, "installed, not loaded");
        std::fs::write(&show, "LoadState=loaded\nActiveState=failed\nMainPID=0\n").unwrap();
        assert_eq!(s.status().await, "loaded, failed");
        std::fs::write(
            &show,
            "LoadState=loaded\nActiveState=activating\nMainPID=0\n",
        )
        .unwrap();
        assert_eq!(s.status().await, "loaded, activating");
    }

    #[tokio::test]
    async fn a_failed_systemctl_is_an_error_naming_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = systemd(dir.path(), "echo Linger=yes");
        s.systemctl = fake(
            dir.path(),
            "nobus",
            "echo 'Failed to connect to bus: No medium found' >&2; exit 1",
        );
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        let err = s
            .install(&exe, 1, "/usr/bin")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("systemctl --user daemon-reload")
                && err.contains("Failed to connect to bus"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn install_refuses_an_empty_path_or_a_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(dir.path(), "echo Linger=yes");
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        assert!(matches!(
            s.install(&exe, 1, " ").await,
            Err(ServiceError::EmptyPath)
        ));
        assert!(matches!(
            s.install(&exe, 1, "/usr/bin\nExecStartPre=/x").await,
            Err(ServiceError::LineBreak)
        ));
        let odd = Systemd {
            provefab_home: dir.path().join("ho\nme"),
            ..systemd(dir.path(), "echo Linger=yes")
        };
        assert!(matches!(
            odd.install(&exe, 1, "/usr/bin").await,
            Err(ServiceError::LineBreak)
        ));
        assert!(!s.binary().exists() && !s.unit_path().exists());
        assert_eq!(calls(dir.path()), "");
    }

    #[tokio::test]
    async fn lingering_off_prints_the_command_and_fails_doctor() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(
            dir.path(),
            "[ \"$1 $2 $3\" = 'show-user alice --property=Linger' ] && echo Linger=no",
        );
        assert_eq!(s.linger().await, Some(false));
        let notes = s.notes().await;
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("sudo loginctl enable-linger alice"),
            "{notes:?}"
        );
        // No unit, no doctor line.
        assert!(s.checks().await.is_empty());
        std::fs::create_dir_all(&s.unit_dir).unwrap();
        std::fs::write(s.unit_path(), "unit").unwrap();
        let c = s.checks().await;
        assert_eq!((c[0].name.as_str(), c[0].ok), ("lingering", false), "{c:?}");
    }

    #[tokio::test]
    async fn lingering_on_is_quiet_and_passes() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(dir.path(), "echo Linger=yes");
        assert_eq!(s.linger().await, Some(true));
        assert!(s.notes().await.is_empty());
        std::fs::create_dir_all(&s.unit_dir).unwrap();
        std::fs::write(s.unit_path(), "unit").unwrap();
        assert!(s.checks().await[0].ok);
    }

    #[tokio::test]
    async fn lingering_unknown_still_names_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let s = systemd(
            dir.path(),
            "echo 'User ID 1000 is not logged in or lingering.' >&2; exit 1",
        );
        assert_eq!(s.linger().await, None);
        let notes = s.notes().await;
        assert!(
            notes[0].contains("could not read")
                && notes[0].contains("sudo loginctl enable-linger alice"),
            "{notes:?}"
        );
        std::fs::create_dir_all(&s.unit_dir).unwrap();
        std::fs::write(s.unit_path(), "unit").unwrap();
        let c = s.checks().await;
        assert!(!c[0].ok && c[0].detail.starts_with("unknown"), "{c:?}");
    }

    #[test]
    fn the_user_is_a_safe_name_or_the_uid() {
        let u = current_user();
        assert!(!u.is_empty());
        assert!(
            u.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)),
            "{u}"
        );
    }
}
