//! Setup and per-run checks for the Codex worker (spec §5.5, D28).
//!
//! The guard hook is a user-level hook in `CODEX_HOME/hooks.json`. Codex only
//! runs user hooks it trusts, so Provefab trusts exactly this one hook
//! through the official app-server (`hooks/list` + `config/batchWrite`, the
//! calls the `/hooks` screen makes). It never uses
//! `--dangerously-bypass-hook-trust`, which would also run a target repo's own
//! `.codex/` hooks. A repo's `.codex/config.toml` is only applied when its
//! directory is marked trusted in `CODEX_HOME/config.toml`, so `check` refuses
//! to run when that is the case.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// Never changes, so the trust hash Codex stores for it stays valid across
/// provefab versions; the binary comes from `PROVEFAB_BIN`. `|| exit 2` makes a
/// missing or crashing guard block the call (Codex lets a failing hook through).
pub const HOOK_COMMAND: &str =
    r#""${PROVEFAB_BIN:-provefab-bin-not-set}" guard --format codex || exit 2"#;
const HOOK_TIMEOUT_SECS: u64 = 30;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum CodexSetupError {
    #[error("codex setup: {0}")]
    Io(#[from] std::io::Error),
    #[error("codex app-server: {0}")]
    Rpc(String),
    #[error("codex: Provefab guard hook in {0} is not trusted; run Provefab's Codex setup")]
    HookNotTrusted(PathBuf),
    #[error(
        "codex: {0} is marked trusted in Provefab's Codex config, so its own .codex/ config would apply; remove that entry"
    )]
    ProjectTrusted(String),
    #[error("codex: {0}")]
    ConfigUnreadable(String),
}

/// The exact `hooks.json` Provefab owns.
pub fn hooks_json() -> String {
    let v = json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": HOOK_COMMAND,
                    "timeout": HOOK_TIMEOUT_SECS,
                }],
            }],
        },
    });
    format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// The key Codex uses for our hook in `hooks/list` and in `hooks.state`.
pub fn hook_key(codex_home: &Path) -> String {
    format!(
        "{}:pre_tool_use:0:0",
        codex_home.join("hooks.json").display()
    )
}

/// Writes `CODEX_HOME/hooks.json` if it is missing or different.
pub fn write_hooks(codex_home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(codex_home)?;
    let path = codex_home.join("hooks.json");
    let want = hooks_json();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
        std::fs::write(&path, want)?;
    }
    Ok(())
}

/// Every project marked `trusted` in a Codex `config.toml`. Provefab owns
/// its `CODEX_HOME`, so any entry is refused: Codex also looks up a linked
/// worktree's trust under its main repository, so matching on the worktree
/// path alone would miss entries that apply. A config that does not parse is
/// an error, never "no trusted projects".
pub fn trusted_projects(config_toml: &str) -> Result<Vec<String>, String> {
    let config = config_toml
        .parse::<toml::Table>()
        .map_err(|e| format!("CODEX_HOME/config.toml does not parse: {e}"))?;
    let Some(projects) = config.get("projects").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    Ok(projects
        .iter()
        .filter(|(_, v)| {
            v.get("trust_level")
                .and_then(toml::Value::as_str)
                .is_some_and(|t| t.eq_ignore_ascii_case("trusted"))
        })
        .map(|(k, _)| k.clone())
        .collect())
}

/// `CODEX_HOME` as Codex sees it: Codex canonicalizes the path, and hook keys
/// are built from the canonical form.
pub fn resolve_home(codex_home: &Path) -> PathBuf {
    codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf())
}

/// Writes the hook file and trusts our hook, and only our hook.
pub async fn setup(codex: &Path, codex_home: &Path, cwd: &Path) -> Result<(), CodexSetupError> {
    write_hooks(codex_home)?;
    let codex_home = &resolve_home(codex_home);
    let mut server = AppServer::start(codex, codex_home).await?;
    let ours = find_our_hook(&mut server, codex_home, cwd).await?;
    if !is_trusted(&ours) {
        let hash = ours
            .get("currentHash")
            .and_then(Value::as_str)
            .ok_or_else(|| CodexSetupError::Rpc("hook has no currentHash".into()))?;
        let key = hook_key(codex_home);
        server
            .request(
                "config/batchWrite",
                json!({
                    "edits": [{
                        "keyPath": "hooks.state",
                        "value": { key: { "trusted_hash": hash } },
                        "mergeStrategy": "upsert",
                    }],
                    "filePath": null,
                    "expectedVersion": null,
                    "reloadUserConfig": true,
                }),
            )
            .await?;
    }
    let ours = find_our_hook(&mut server, codex_home, cwd).await?;
    server.stop().await;
    if is_trusted(&ours) {
        Ok(())
    } else {
        Err(CodexSetupError::HookNotTrusted(
            codex_home.join("hooks.json"),
        ))
    }
}

/// Directories Codex may key project trust on for this worktree: the worktree
/// itself and, for a linked git worktree, its main repository (Codex resolves
/// trust to the root git project).
pub fn trust_targets(worktree: &Path) -> Vec<PathBuf> {
    let wt = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    let mut targets = vec![wt.clone()];
    let common = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(&wt)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    if let Some(main) = common
        .and_then(|c| c.parent().map(Path::to_path_buf))
        .and_then(|m| m.canonicalize().ok())
        && main != wt
    {
        targets.push(main);
    }
    targets
}

/// Before every Codex stage. Codex marks a project trusted by itself (and
/// then applies its `.codex/` config) when the project has no trust level
/// and the sandbox can write to it, which `workspace-write` implement stages
/// can. Pinning `untrusted` first stops that: Codex only auto-trusts a
/// project whose level is unset (spec D31).
pub async fn pin_untrusted(
    codex: &Path,
    codex_home: &Path,
    worktree: &Path,
) -> Result<(), CodexSetupError> {
    let codex_home = &resolve_home(codex_home);
    let value: serde_json::Map<String, Value> = trust_targets(worktree)
        .into_iter()
        .map(|p| {
            (
                p.display().to_string(),
                json!({ "trust_level": "untrusted" }),
            )
        })
        .collect();
    let mut server = AppServer::start(codex, codex_home).await?;
    let written = server
        .request(
            "config/batchWrite",
            json!({
                "edits": [{
                    "keyPath": "projects",
                    "value": value,
                    "mergeStrategy": "upsert",
                }],
                "filePath": null,
                "expectedVersion": null,
                "reloadUserConfig": true,
            }),
        )
        .await;
    server.stop().await;
    written.map(|_| ())
}

/// Before every Codex stage: our hook is trusted and enabled, and the
/// provefab's Codex config marks no project trusted.
pub async fn check(codex: &Path, codex_home: &Path, cwd: &Path) -> Result<(), CodexSetupError> {
    let codex_home = &resolve_home(codex_home);
    let config = match std::fs::read_to_string(codex_home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(CodexSetupError::Io(e)),
    };
    let trusted = trusted_projects(&config).map_err(CodexSetupError::ConfigUnreadable)?;
    if let Some(p) = trusted.into_iter().next() {
        return Err(CodexSetupError::ProjectTrusted(p));
    }
    let mut server = AppServer::start(codex, codex_home).await?;
    let ours = find_our_hook(&mut server, codex_home, cwd).await;
    server.stop().await;
    match ours {
        Ok(h) if is_trusted(&h) => Ok(()),
        _ => Err(CodexSetupError::HookNotTrusted(
            codex_home.join("hooks.json"),
        )),
    }
}

fn is_trusted(hook: &Value) -> bool {
    hook.get("trustStatus").and_then(Value::as_str) == Some("trusted")
        && hook.get("enabled").and_then(Value::as_bool) != Some(false)
}

/// Our hook as `hooks/list` reports it: the right key, a user-level source,
/// and exactly our command. Anything else is not ours and is never trusted.
async fn find_our_hook(
    server: &mut AppServer,
    codex_home: &Path,
    cwd: &Path,
) -> Result<Value, CodexSetupError> {
    let listed = server
        .request("hooks/list", json!({ "cwds": [cwd] }))
        .await?;
    let key = hook_key(codex_home);
    listed
        .pointer("/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .find(|h| {
            h.get("key").and_then(Value::as_str) == Some(key.as_str())
                && h.get("source").and_then(Value::as_str) == Some("user")
                && h.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
        })
        .ok_or(CodexSetupError::HookNotTrusted(
            codex_home.join("hooks.json"),
        ))
}

/// Minimal JSON-RPC client for `codex app-server` over stdio.
struct AppServer {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl AppServer {
    async fn start(codex: &Path, codex_home: &Path) -> Result<Self, CodexSetupError> {
        let mut child = Command::new(codex)
            .arg("app-server")
            .env("CODEX_HOME", codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CodexSetupError::Rpc("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CodexSetupError::Rpc("no stdout".into()))?;
        let mut server = Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
        };
        server
            .request(
                "initialize",
                json!({"clientInfo": {"name": "provefab", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        server.send(&json!({"method": "initialized"})).await?;
        Ok(server)
    }

    async fn send(&mut self, msg: &Value) -> Result<(), CodexSetupError> {
        self.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, CodexSetupError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        let wait = async {
            while let Some(line) = self.lines.next_line().await? {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if msg.get("id").and_then(Value::as_u64) != Some(id) {
                    continue; // notifications and other traffic
                }
                if let Some(err) = msg.get("error") {
                    return Err(CodexSetupError::Rpc(format!("{method}: {err}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            Err(CodexSetupError::Rpc(format!("{method}: app-server closed")))
        };
        tokio::time::timeout(RPC_TIMEOUT, wait)
            .await
            .map_err(|_| CodexSetupError::Rpc(format!("{method}: timed out")))?
    }

    async fn stop(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_json_runs_the_guard_on_every_tool_and_fails_closed() {
        let v: Value = serde_json::from_str(&hooks_json()).unwrap();
        let h = &v["hooks"]["PreToolUse"][0];
        assert_eq!(h["matcher"], "*");
        assert_eq!(h["hooks"][0]["type"], "command");
        assert_eq!(h["hooks"][0]["timeout"], 30);
        let cmd = h["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("guard --format codex") && cmd.ends_with("|| exit 2"),
            "{cmd}"
        );
    }

    #[test]
    fn hook_key_matches_what_codex_reports() {
        assert_eq!(
            hook_key(Path::new("/Users/me/.provefab/codex")),
            "/Users/me/.provefab/codex/hooks.json:pre_tool_use:0:0"
        );
    }

    #[test]
    fn write_hooks_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        write_hooks(home.path()).unwrap();
        let first = std::fs::metadata(home.path().join("hooks.json"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_hooks(home.path()).unwrap();
        let second = std::fs::metadata(home.path().join("hooks.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first, second, "unchanged content must not be rewritten");
    }

    /// Final review #4: Codex also looks up trust under a linked worktree's main
    /// repository, so any trusted project in Provefab's own CODEX_HOME is refused.
    #[test]
    fn review_4_every_trusted_project_is_reported() {
        let config = "[projects.\"/Users/me/app\"]\ntrust_level = \"trusted\"\n\n[projects.\"/elsewhere\"]\ntrust_level = \"trusted\"\n\n[projects.\"/w\"]\ntrust_level = \"untrusted\"\n";
        let mut found = trusted_projects(config).unwrap();
        found.sort();
        assert_eq!(
            found,
            vec!["/Users/me/app".to_string(), "/elsewhere".to_string()]
        );
        assert!(trusted_projects("").unwrap().is_empty());
    }

    /// Final review #10: a config Provefab cannot read is not "no trusted projects".
    #[test]
    fn review_10_unparseable_config_is_an_error() {
        assert!(trusted_projects("not toml [[[").is_err());
    }

    /// Final review #7: Codex canonicalizes CODEX_HOME, so hook keys must use the real path.
    #[test]
    fn review_7_codex_home_is_canonicalized() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-home");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link-home");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(resolve_home(&link), real.canonicalize().unwrap());
        assert_eq!(
            hook_key(&resolve_home(&link)),
            format!(
                "{}/hooks.json:pre_tool_use:0:0",
                real.canonicalize().unwrap().display()
            )
        );
    }

    /// Codex keys trust on the main repository of a linked worktree, so both the
    /// worktree and its main repository must be pinned untrusted.
    #[test]
    fn trust_targets_include_a_linked_worktree_main_repository() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str], cwd: &Path| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        let main = dir.path().join("main");
        std::fs::create_dir(&main).unwrap();
        run(&["init", "-q"], &main);
        run(
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
            &main,
        );
        let linked = dir.path().join("linked");
        run(&["worktree", "add", "-q", linked.to_str().unwrap()], &main);
        let targets = trust_targets(&linked);
        assert!(
            targets.contains(&linked.canonicalize().unwrap()),
            "{targets:?}"
        );
        assert!(
            targets.contains(&main.canonicalize().unwrap()),
            "{targets:?}"
        );
        let plain = trust_targets(&main);
        assert_eq!(plain, vec![main.canonicalize().unwrap()]);
    }
}
