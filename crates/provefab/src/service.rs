//! `provefab service`: run `provefab run` as a launchd agent, independent of any
//! terminal or Claude session (spec §3.6, D47).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// The launchd label, also the plist file name.
pub const LABEL: &str = "dev.provefab.run";

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("service: {0}: {1}")]
    Io(String, String),
    #[error("service: launchctl {args}: {message}")]
    Launchctl { args: String, message: String },
    #[error("PATH is empty; run from a normal terminal")]
    EmptyPath,
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent definition: the stable binary, `run --workers N`, the frozen
/// `PATH` (launchd starts agents with a minimal one), Provefab home, and
/// both output streams appended to `<home>/logs/run.log`.
pub fn plist(bin: &Path, workers: usize, path_env: &str, provefab_home: &Path) -> String {
    let log = provefab_home.join("logs").join("run.log");
    let s = |p: &Path| xml(&p.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>run</string>
    <string>--workers</string>
    <string>{workers}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>PROVEFAB_HOME</key>
    <string>{home}</string>
  </dict>
  <key>WorkingDirectory</key>
  <string>{home}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        bin = s(bin),
        path = xml(path_env),
        home = s(provefab_home),
        log = s(&log),
    )
}

/// Where things go; tests point these at temporary directories and a fake `launchctl`.
pub struct Service {
    pub launchctl: PathBuf,
    /// `~/Library/LaunchAgents`.
    pub agents_dir: PathBuf,
    pub provefab_home: PathBuf,
    pub uid: u32,
    /// Delay between `bootstrap` retries; injectable so tests don't sleep.
    pub bootstrap_retry_delay: Duration,
}

impl Service {
    pub fn for_user(provefab_home: &Path) -> Self {
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Self {
            launchctl: "launchctl".into(),
            agents_dir: user_home.join("Library").join("LaunchAgents"),
            provefab_home: provefab_home.to_path_buf(),
            // SAFETY: getuid has no preconditions and cannot fail.
            uid: unsafe { libc::getuid() },
            bootstrap_retry_delay: Duration::from_secs(1),
        }
    }

    pub fn plist_path(&self) -> PathBuf {
        self.agents_dir.join(format!("{LABEL}.plist"))
    }

    /// The copy launchd runs: rebuilding the checkout never pulls it away (D47).
    pub fn binary(&self) -> PathBuf {
        self.provefab_home.join("bin").join("provefab")
    }

    async fn launchctl(&self, args: &[&str]) -> Result<String, ServiceError> {
        let out = Command::new(&self.launchctl)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| ServiceError::Launchctl {
                args: args.join(" "),
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
            Err(ServiceError::Launchctl {
                args: args.join(" "),
                message: text.trim().to_string(),
            })
        }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    /// `bootout` completes asynchronously on macOS, so an immediate `bootstrap`
    /// can race it (error 5); retry up to 5 times before giving up.
    async fn bootstrap_with_retry(
        &self,
        domain: &str,
        plist_path: &str,
    ) -> Result<String, ServiceError> {
        let mut last_err = None;
        for attempt in 0..5 {
            match self.launchctl(&["bootstrap", domain, plist_path]).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 4 {
                        tokio::time::sleep(self.bootstrap_retry_delay).await;
                    }
                }
            }
        }
        Err(last_err.expect("loop runs at least once"))
    }

    /// Copies `exe` to the stable path, writes the plist and (re)loads the agent.
    pub async fn install(
        &self,
        exe: &Path,
        workers: usize,
        path_env: &str,
    ) -> Result<PathBuf, ServiceError> {
        use std::os::unix::fs::PermissionsExt;
        if path_env.trim().is_empty() {
            return Err(ServiceError::EmptyPath);
        }
        let io = |what: &Path, e: std::io::Error| {
            ServiceError::Io(what.display().to_string(), e.to_string())
        };
        let bin = self.binary();
        for dir in [
            bin.parent().unwrap_or(&self.provefab_home).to_path_buf(),
            self.provefab_home.join("logs"),
            self.agents_dir.clone(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        }
        // launchd holds the log file open for the life of the agent, so rotation
        // can only safely happen here, before `launchctl bootstrap` (re)starts it.
        let log = self.provefab_home.join("logs").join("run.log");
        if let Ok(meta) = std::fs::metadata(&log) {
            const ROTATE_AT: u64 = 10 * 1024 * 1024;
            if meta.len() > ROTATE_AT {
                let rotated = log.with_file_name("run.log.1");
                std::fs::rename(&log, &rotated).map_err(|e| io(&log, e))?;
            }
        }
        // Copy to a temporary name, then rename: a running agent keeps its old inode.
        let tmp = bin.with_extension("new");
        std::fs::copy(exe, &tmp).map_err(|e| io(&tmp, e))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io(&tmp, e))?;
        std::fs::rename(&tmp, &bin).map_err(|e| io(&bin, e))?;
        let plist_path = self.plist_path();
        std::fs::write(
            &plist_path,
            plist(&bin, workers, path_env, &self.provefab_home),
        )
        .map_err(|e| io(&plist_path, e))?;
        // A previous version may be loaded: unload it first (not loaded is fine).
        let target = format!("{}/{LABEL}", self.domain());
        let _ = self.launchctl(&["bootout", &target]).await;
        let p = plist_path.display().to_string();
        self.bootstrap_with_retry(&self.domain(), &p).await?;
        Ok(plist_path)
    }

    pub async fn uninstall(&self) -> Result<(), ServiceError> {
        let target = format!("{}/{LABEL}", self.domain());
        let _ = self.launchctl(&["bootout", &target]).await;
        let p = self.plist_path();
        if p.exists() {
            std::fs::remove_file(&p)
                .map_err(|e| ServiceError::Io(p.display().to_string(), e.to_string()))?;
        }
        Ok(())
    }

    /// `running (pid N)`, `loaded, not running`, `installed, not loaded`, or
    /// `not installed`.
    pub async fn status(&self) -> String {
        let target = format!("{}/{LABEL}", self.domain());
        match self.launchctl(&["print", &target]).await {
            Err(_) => {
                if self.plist_path().exists() {
                    "installed, not loaded".into()
                } else {
                    "not installed".into()
                }
            }
            Ok(text) => {
                let field = |key: &str| {
                    text.lines()
                        .map(str::trim)
                        .find_map(|l| l.strip_prefix(key))
                        .map(|v| v.trim().to_string())
                };
                match (field("state = "), field("pid = ")) {
                    (Some(state), Some(pid)) if state == "running" => {
                        format!("running (pid {pid})")
                    }
                    (Some(state), _) => format!("loaded, {state}"),
                    _ => "loaded".into(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn service_plist_runs_the_stable_binary_with_path_and_logs() {
        let p = plist(
            Path::new("/Users/a/.provefab/bin/provefab"),
            2,
            "/opt/homebrew/bin:/usr/bin",
            Path::new("/Users/a/.provefab"),
        );
        for expected in [
            "<string>dev.provefab.run</string>",
            "<string>/Users/a/.provefab/bin/provefab</string>\n    <string>run</string>\n    <string>--workers</string>\n    <string>2</string>",
            "<key>PATH</key>\n    <string>/opt/homebrew/bin:/usr/bin</string>",
            "<key>PROVEFAB_HOME</key>\n    <string>/Users/a/.provefab</string>",
            "<key>KeepAlive</key>\n  <true/>",
            "<key>StandardErrorPath</key>\n  <string>/Users/a/.provefab/logs/run.log</string>",
        ] {
            assert!(p.contains(expected), "missing {expected:?} in\n{p}");
        }
        assert!(plist(Path::new("/a&b"), 1, "x<y", Path::new("/h")).contains("/a&amp;b"));
    }

    fn service(dir: &Path) -> Service {
        let fake = dir.join("launchctl");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\nif [ \"$1\" = print ]; then printf '\\tstate = running\\n\\tpid = 42\\n'; fi\n",
                dir.join("calls.txt").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        Service {
            launchctl: fake,
            agents_dir: dir.join("LaunchAgents"),
            provefab_home: dir.join("home"),
            uid: 501,
            bootstrap_retry_delay: Duration::ZERO,
        }
    }

    fn service_with_failing_print(dir: &Path) -> Service {
        let fake = dir.join("launchctl");
        std::fs::write(&fake, "#!/bin/sh\nif [ \"$1\" = print ]; then exit 1; fi\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        Service {
            launchctl: fake,
            agents_dir: dir.join("LaunchAgents"),
            provefab_home: dir.join("home"),
            uid: 501,
            bootstrap_retry_delay: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn status_reports_not_loaded_when_plist_exists_but_print_fails() {
        let dir = tempfile::tempdir().unwrap();
        let s = service_with_failing_print(dir.path());
        std::fs::create_dir_all(&s.agents_dir).unwrap();
        std::fs::write(s.plist_path(), "plist").unwrap();
        assert_eq!(s.status().await, "installed, not loaded");
    }

    #[tokio::test]
    async fn status_reports_not_installed_when_plist_is_absent_and_print_fails() {
        let dir = tempfile::tempdir().unwrap();
        let s = service_with_failing_print(dir.path());
        assert_eq!(s.status().await, "not installed");
    }

    #[tokio::test]
    async fn install_copies_the_binary_and_bootstraps() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        let plist_path = s.install(&exe, 3, "/usr/bin").await.unwrap();
        assert_eq!(std::fs::read_to_string(s.binary()).unwrap(), "binary");
        let mode = std::fs::metadata(s.binary()).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
        assert!(
            std::fs::read_to_string(&plist_path)
                .unwrap()
                .contains("<string>3</string>")
        );
        assert!(dir.path().join("home/logs").is_dir());
        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap();
        assert_eq!(
            calls,
            format!(
                "bootout gui/501/dev.provefab.run\nbootstrap gui/501 {}\n",
                plist_path.display()
            )
        );
        assert_eq!(s.status().await, "running (pid 42)");
        s.uninstall().await.unwrap();
        assert!(!plist_path.exists());
    }

    #[tokio::test]
    async fn install_retries_bootstrap_after_bootout_race() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls.txt");
        let counter = dir.path().join("bootstrap-count.txt");
        let fake = dir.path().join("launchctl");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> {calls}\nif [ \"$1\" = bootstrap ]; then\n  n=$(cat {counter} 2>/dev/null || echo 0)\n  n=$((n+1))\n  echo $n > {counter}\n  if [ $n -le 2 ]; then\n    exit 5\n  fi\nfi\n",
                calls = calls.display(),
                counter = counter.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = Service {
            launchctl: fake,
            agents_dir: dir.path().join("LaunchAgents"),
            provefab_home: dir.path().join("home"),
            uid: 501,
            bootstrap_retry_delay: Duration::ZERO,
        };
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        let plist_path = s.install(&exe, 3, "/usr/bin").await.unwrap();
        let calls_text = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(
            calls_text,
            format!(
                "bootout gui/501/dev.provefab.run\nbootstrap gui/501 {p}\nbootstrap gui/501 {p}\nbootstrap gui/501 {p}\n",
                p = plist_path.display()
            )
        );
    }

    #[tokio::test]
    async fn install_rotates_a_large_run_log() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let logs = dir.path().join("home/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let big = vec![b'x'; 11 * 1024 * 1024];
        std::fs::write(logs.join("run.log"), &big).unwrap();
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        s.install(&exe, 1, "/usr/bin").await.unwrap();
        assert!(!logs.join("run.log").exists());
        assert_eq!(std::fs::read(logs.join("run.log.1")).unwrap(), big);
    }

    #[tokio::test]
    async fn install_leaves_a_small_run_log_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let logs = dir.path().join("home/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("run.log"), "small").unwrap();
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();
        s.install(&exe, 1, "/usr/bin").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(logs.join("run.log")).unwrap(),
            "small"
        );
        assert!(!logs.join("run.log.1").exists());
    }

    #[tokio::test]
    async fn install_refuses_an_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(dir.path());
        let exe = dir.path().join("built");
        std::fs::write(&exe, "binary").unwrap();

        assert!(s.install(&exe, 3, "").await.is_err());
        assert!(s.install(&exe, 3, "   ").await.is_err());

        assert!(!s.binary().exists());
        assert!(!s.plist_path().exists());
    }
}
