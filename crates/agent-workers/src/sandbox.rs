use crate::{StageRequest, ToolProfile};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Explicit command restrictions. The localhost exception is for gates only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub allow_local_network: bool,
    pub deny_read: Vec<PathBuf>,
    pub deny_write: Vec<PathBuf>,
    pub runtime: PathBuf,
}
impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            allow_local_network: false,
            deny_read: Vec::new(),
            deny_write: Vec::new(),
            runtime: "srt".into(),
        }
    }
}
impl SandboxPolicy {
    pub(crate) fn codex_filesystem(&self, req: &StageRequest, auth: &std::path::Path) -> String {
        let mut entries = BTreeMap::new();
        entries.insert(":root".to_string(), "read");
        if req.tools == ToolProfile::Full {
            entries.insert(req.cwd.to_string_lossy().to_string(), "write");
        }
        for path in self.deny_write.iter().chain([&req.session_dir]) {
            entries.insert(path.to_string_lossy().to_string(), "read");
        }
        for name in [".git", ".claude", ".codex", ".pi"] {
            entries.insert(req.cwd.join(name).to_string_lossy().to_string(), "read");
        }
        entries.insert(
            req.session_dir
                .join("sandbox-tmp")
                .to_string_lossy()
                .to_string(),
            "write",
        );
        // More-specific native entries must never weaken an explicit ancestor denial.
        for (path, permission) in &mut entries {
            if *permission == "write"
                && self
                    .deny_write
                    .iter()
                    .any(|denied| std::path::Path::new(path).starts_with(denied))
            {
                *permission = "read";
            }
        }
        for path in self.deny_read.iter().map(PathBuf::as_path).chain([auth]) {
            entries.retain(|p, _| !std::path::Path::new(p).starts_with(path));
            entries.insert(path.to_string_lossy().to_string(), "deny");
        }
        format!(
            "permissions.provefab.filesystem={{{}}}",
            entries
                .iter()
                .map(|(p, v)| format!(
                    "{}={}",
                    serde_json::to_string(p).unwrap(),
                    serde_json::to_string(v).unwrap()
                ))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

/// Trusted, normalized JSON policy also consumed by Provefab's direct-tool guard.
pub const SANDBOX_POLICY_ENV: &str = "PROVEFAB_SANDBOX_POLICY";
pub const SANDBOX_RUNTIME_ENV: &str = "PROVEFAB_SANDBOX_RUNTIME";

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into())
}

/// Resolve existing ancestors too, so an absent leaf cannot hide a symlink escape.
pub(crate) fn canonical_path(path: &std::path::Path) -> std::io::Result<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(invalid(format!(
            "sandbox path must be absolute without '..': {}",
            path.display()
        )));
    }
    // Native Claude treats these as patterns. Refuse ambiguity instead of widening a rule.
    if path
        .to_str()
        .is_none_or(|p| p.contains(['*', '?', '[', ']']) || p.contains('\0'))
    {
        return Err(invalid(
            "sandbox paths must be UTF-8 literal paths without glob metacharacters",
        ));
    }
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(e)?;
            Ok(canonical_path(parent)?.join(
                path.file_name()
                    .ok_or_else(|| invalid("missing path name"))?,
            ))
        }
        Err(e) => Err(e),
    }
}
fn disjoint(a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
    if a.starts_with(b) || b.starts_with(a) {
        return Err(invalid(format!(
            "sandbox trusted path {} overlaps writable root {}",
            b.display(),
            a.display()
        )));
    }
    Ok(())
}

/// Only operational values are inherited. Credentials remain in protected CLI login files.
pub fn sanitize_environment(cmd: &mut tokio::process::Command) {
    for (key, _) in std::env::vars_os() {
        if !safe_env_key(&key.to_string_lossy()) {
            cmd.env_remove(key);
        }
    }
    cmd.env("GIT_CONFIG_COUNT", "0");
}
fn safe_env_key(key: &str) -> bool {
    matches!(
        key,
        "HOME"
            | "PATH"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "TERM"
            | "COLORTERM"
            | "TZ"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "SYSTEMROOT"
            | "XDG_CONFIG_HOME"
            | "XDG_CACHE_HOME"
            | "XDG_DATA_HOME"
            | "PI_CODING_AGENT_DIR"
    )
}
pub(crate) fn shell_environment() -> BTreeMap<String, String> {
    std::env::vars().filter(|(k, _)| safe_env_key(k)).collect()
}
pub(crate) fn codex_shell_environment(req: &StageRequest) -> String {
    let mut env = shell_environment();
    for key in ["TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"] {
        env.insert(
            key.into(),
            req.session_dir.join("sandbox-tmp").to_string_lossy().into(),
        );
    }
    let set = env
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                serde_json::to_string(k).unwrap(),
                serde_json::to_string(v).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "shell_environment_policy={{inherit=\"none\",ignore_default_excludes=false,exclude=[],include_only=[],set={{{set}}}}}"
    )
}

// Canonical-only deny rules protect a symlink target but allow replacing the link itself.
// System aliases before the worktree (macOS /tmp and /var) remain valid.
fn reject_worktree_symlink(
    path: &std::path::Path,
    worktree: &std::path::Path,
) -> std::io::Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component);
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if prefix.parent().is_some_and(|parent| {
                    parent.canonicalize().is_ok_and(|p| p.starts_with(worktree))
                }) {
                    return Err(invalid(format!(
                        "protected worktree path must not traverse a symlink: {}",
                        path.display()
                    )));
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub(crate) fn prepare_request(
    req: &StageRequest,
    trusted: &[PathBuf],
    secrets: &[PathBuf],
) -> std::io::Result<StageRequest> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(invalid("native sandbox unsupported on this platform"));
    }
    let mut out = req.clone();
    out.cwd = req.cwd.canonicalize()?;
    if !out.cwd.is_dir() {
        return Err(invalid("sandbox worktree is not a directory"));
    }
    out.session_dir = canonical_path(&req.session_dir)?;
    disjoint(&out.cwd, &out.session_dir)?;
    for path in trusted.iter().chain(secrets) {
        reject_worktree_symlink(path, &out.cwd)?;
        reject_worktree_symlink(path, &out.session_dir.join("sandbox-tmp"))?;
        let path = canonical_path(path)?;
        disjoint(&out.cwd, &path)?;
        disjoint(&out.session_dir.join("sandbox-tmp"), &path)?;
        out.sandbox.deny_write.push(path);
    }
    out.sandbox.deny_read.extend_from_slice(secrets);
    for name in [".git", ".claude", ".codex", ".pi"] {
        out.sandbox.deny_write.push(out.cwd.join(name));
    }
    // These convenience grants are native-runtime defaults, not requested workspace access.
    for path in ["/tmp/claude", "/private/tmp/claude"] {
        out.sandbox.deny_write.push(path.into());
    }
    // Native Claude may fall back to this shared directory for long TMPDIR paths.
    let uid = unsafe { libc::getuid() };
    for root in ["/tmp", "/private/tmp"] {
        out.sandbox
            .deny_write
            .push(PathBuf::from(root).join(format!("claude-{uid}")));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for path in [".npm/_logs", ".claude/debug"] {
            out.sandbox.deny_write.push(home.join(path));
        }
        for path in [
            ".codex",
            ".claude",
            ".claude.json",
            ".pi/agent",
            ".ssh",
            ".aws",
            ".config/gcloud",
            ".config/gh",
            ".git-credentials",
            ".netrc",
            ".npmrc",
        ] {
            out.sandbox.deny_read.push(home.join(path));
        }
    }
    if let Some(auth) = std::env::var_os("PI_CODING_AGENT_DIR") {
        out.sandbox.deny_read.push(PathBuf::from(auth));
    }
    for path in &out.sandbox.deny_write {
        reject_worktree_symlink(path, &out.cwd)?;
    }
    for list in [&mut out.sandbox.deny_read, &mut out.sandbox.deny_write] {
        *list = list
            .iter()
            .map(|p| canonical_path(p))
            .collect::<std::io::Result<Vec<_>>>()?;
        list.sort();
        list.dedup();
    }
    for denied in &out.sandbox.deny_read {
        if out.cwd.starts_with(denied) || out.session_dir.join("sandbox-tmp").starts_with(denied) {
            return Err(invalid(
                "sandbox read denial overlaps a writable workspace or temporary root",
            ));
        }
    }
    if out
        .sandbox
        .deny_write
        .iter()
        .any(|denied| out.session_dir.join("sandbox-tmp").starts_with(denied))
    {
        return Err(invalid(
            "sandbox temporary root overlaps an explicit write denial",
        ));
    }
    std::fs::create_dir_all(&out.session_dir)?;
    out.session_dir = out.session_dir.canonicalize()?;
    disjoint(&out.cwd, &out.session_dir)?;
    let temporary = out.session_dir.join("sandbox-tmp");
    if canonical_path(&temporary)? != temporary {
        return Err(invalid("sandbox temporary directory must not be a symlink"));
    }
    std::fs::create_dir_all(&temporary)?;
    Ok(out)
}
impl SandboxPolicy {
    pub(crate) fn json(&self, req: &StageRequest, gates: bool) -> serde_json::Value {
        serde_json::json!({
            "network": {"allowedDomains": [], "deniedDomains": ["*"], "strictAllowedDomains": true, "allowLocalBinding": gates && self.allow_local_network, "allowAllUnixSockets": false},
            "filesystem": {"denyRead": self.deny_read, "allowWrite": if req.tools == ToolProfile::Full { vec![req.cwd.clone(), req.session_dir.join("sandbox-tmp")] } else { vec![req.session_dir.join("sandbox-tmp")] }, "denyWrite": self.deny_write},
            "enableWeakerNestedSandbox": false,
            "enableWeakerNetworkIsolation": false
        })
    }
    pub(crate) fn claude_settings(
        &self,
        req: &StageRequest,
        auth: &std::path::Path,
    ) -> serde_json::Value {
        let mut policy = self.json(req, false);
        policy["filesystem"]["denyRead"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(auth));
        if req.tools != ToolProfile::Full {
            policy["filesystem"]["denyWrite"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!(req.cwd));
        }
        policy["network"]
            .as_object_mut()
            .unwrap()
            .remove("strictAllowedDomains");
        policy["network"]["strictAllowlist"] = true.into();
        policy["enabled"] = true.into();
        policy["failIfUnavailable"] = true.into();
        policy["allowUnsandboxedCommands"] = false.into();
        policy["excludedCommands"] = serde_json::json!([]);
        serde_json::json!({"sandbox": policy})
    }
}

pub(crate) fn write_policy(req: &StageRequest, gates: bool) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = req.session_dir.join("sandbox-policy.json");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.write_all(req.sandbox.json(req, gates).to_string().as_bytes())?;
    Ok(path)
}

pub(crate) fn executable(
    program: &std::path::Path,
    worktree: &std::path::Path,
    scratch: &std::path::Path,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = if program.components().count() > 1 || program.is_absolute() {
        program.to_path_buf()
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(program))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                invalid(format!(
                    "required sandbox executable {} not found",
                    program.display()
                ))
            })?
    };
    reject_worktree_symlink(&path, worktree)?;
    reject_worktree_symlink(&path, &scratch.join("sandbox-tmp"))?;
    let path = path.canonicalize()?;
    disjoint(worktree, &path)?;
    disjoint(&scratch.join("sandbox-tmp"), &path)?;
    if !path.is_file() || path.metadata()?.permissions().mode() & 0o111 == 0 {
        return Err(invalid("sandbox runtime is not executable"));
    }
    Ok(path)
}

pub(crate) fn check_version(program: &std::path::Path, minimum: [u64; 3]) -> std::io::Result<()> {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(program);
    cmd.arg("--version")
        .env_clear()
        .envs(shell_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("sandbox runtime version probe timed out"));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output()?;
    let version = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|v| {
            let values: Vec<u64> = v
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            <[u64; 3]>::try_from(values).ok()
        });
    if !output.status.success() || version.is_none_or(|v| v < minimum) {
        return Err(invalid(format!(
            "{} requires a verified native sandbox version >= {}.{}.{}; version probe failed or unsupported",
            program.display(),
            minimum[0],
            minimum[1],
            minimum[2]
        )));
    }
    Ok(())
}

/// Gate commands use SRT, with the sole opt-in localhost exception. Scratch must be trusted.
pub fn gate_command(
    worktree: &std::path::Path,
    scratch: &std::path::Path,
    policy: &SandboxPolicy,
    command: &str,
) -> std::io::Result<tokio::process::Command> {
    let request = StageRequest {
        sandbox: policy.clone(),
        cwd: worktree.into(),
        session_dir: scratch.into(),
        tools: ToolProfile::Full,
        prompt: String::new(),
        model: String::new(),
        provider: None,
        system_prompt_file: None,
        output_schema: None,
        max_turns: 0,
        timeout: std::time::Duration::ZERO,
    };
    let mut req = prepare_request(&request, &[], &[])?;
    let runtime = executable(&policy.runtime, &req.cwd, &req.session_dir)?;
    check_version(&runtime, [0, 0, 79])?;
    req.sandbox.deny_write.push(runtime.clone());
    let config = write_policy(&req, true)?;
    let mut cmd = tokio::process::Command::new(runtime);
    cmd.args([
        std::ffi::OsStr::new("--settings"),
        config.as_os_str(),
        std::ffi::OsStr::new("--"),
        std::ffi::OsStr::new("/bin/sh"),
        std::ffi::OsStr::new("-c"),
        std::ffi::OsStr::new(command),
    ])
    .current_dir(&req.cwd);
    sanitize_environment(&mut cmd);
    apply_temporary_env(&mut cmd, &req);
    cmd.env(SANDBOX_POLICY_ENV, config);
    Ok(cmd)
}

pub(crate) fn apply_temporary_env(cmd: &mut tokio::process::Command, req: &StageRequest) {
    for key in ["TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"] {
        cmd.env(key, req.session_dir.join("sandbox-tmp"));
    }
}

/// Array merges can re-enable excluded commands. Reject security overrides instead of merging.
pub(crate) fn check_claude_settings(config_dir: &std::path::Path) -> std::io::Result<()> {
    let mut paths = vec![
        config_dir.join("settings.json"),
        config_dir.join("managed-settings.json"),
    ];
    for root in [
        "/Library/Application Support/ClaudeCode",
        "/etc/claude-code",
    ] {
        paths.push(PathBuf::from(root).join("managed-settings.json"));
        let directory = PathBuf::from(root).join("managed-settings.d");
        if directory.exists() {
            for entry in std::fs::read_dir(directory)? {
                let path = entry?.path();
                if path.extension().is_some_and(|v| v == "json") {
                    paths.push(path);
                }
            }
        }
    }
    for path in paths {
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let settings: serde_json::Value = serde_json::from_slice(&data)
            .map_err(|e| invalid(format!("invalid Claude settings {}: {e}", path.display())))?;
        if settings.get("sandbox").is_some()
            || settings
                .pointer("/permissions/additionalDirectories")
                .is_some()
            || settings.get("env").is_some()
        {
            return Err(invalid(format!(
                "Claude settings {} can widen the sandbox; remove sandbox, permissions.additionalDirectories and env overrides from this dedicated worker profile",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn request(root: &Path) -> StageRequest {
        let work = root.join("work ' \" $ \\ é");
        std::fs::create_dir_all(&work).unwrap();
        StageRequest {
            sandbox: SandboxPolicy::default(),
            cwd: work,
            session_dir: root.join("session"),
            tools: ToolProfile::Full,
            prompt: String::new(),
            model: String::new(),
            provider: None,
            system_prompt_file: None,
            output_schema: None,
            max_turns: 1,
            timeout: std::time::Duration::ZERO,
        }
    }
    #[test]
    fn policy_allows_only_workspace_and_private_temporary_writes() {
        let root = tempfile::tempdir().unwrap();
        let req = prepare_request(&request(root.path()), &[], &[]).unwrap();
        let p = req.sandbox.json(&req, false);
        assert_eq!(
            p["filesystem"]["allowWrite"],
            serde_json::json!([req.cwd, req.session_dir.join("sandbox-tmp")])
        );
        assert!(
            !p["filesystem"]["denyWrite"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(req.session_dir))
        );
        let mut readonly = req.clone();
        readonly.tools = ToolProfile::ReadOnly;
        assert_eq!(
            readonly.sandbox.json(&readonly, false)["filesystem"]["allowWrite"],
            serde_json::json!([req.session_dir.join("sandbox-tmp")])
        );
    }
    #[test]
    fn policy_worker_network_stays_strict_and_paths_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let mut request = request(root.path());
        request.sandbox.allow_local_network = true;
        request
            .sandbox
            .deny_read
            .push(root.path().join("secret ' \" $ \\ é"));
        let req = prepare_request(&request, &[], &[]).unwrap();
        let path = write_policy(&req, false).unwrap();
        let p: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(p["network"]["allowLocalBinding"], false);
        assert_eq!(p["network"]["strictAllowedDomains"], true);
        assert_eq!(p["network"]["deniedDomains"], serde_json::json!(["*"]));
        assert_eq!(
            req.sandbox.json(&req, true)["network"]["allowLocalBinding"],
            true
        );
        assert!(
            p["filesystem"]["denyRead"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(req.sandbox.deny_read[0]))
        );
    }
    #[test]
    fn preparation_rejects_symlink_and_ancestor_overlap() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&req.cwd, &alias).unwrap();
        assert!(prepare_request(&req, &[alias.join("config")], &[]).is_err());
        assert!(prepare_request(&req, &[root.path().into()], &[]).is_err());
    }
    #[test]
    fn policy_file_symlink_cannot_overwrite_a_trusted_file() {
        let root = tempfile::tempdir().unwrap();
        let req = prepare_request(&request(root.path()), &[], &[]).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, req.session_dir.join("sandbox-policy.json")).unwrap();
        assert!(write_policy(&req, false).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
    }
    #[test]
    fn missing_gate_runtime_never_executes_payload() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let policy = SandboxPolicy {
            runtime: root.path().join("absent"),
            ..Default::default()
        };
        assert!(gate_command(&req.cwd, &req.session_dir, &policy, "touch marker").is_err());
        assert!(!req.cwd.join("marker").exists());
    }
    #[test]
    fn claude_rejects_mergeable_security_settings_but_preserves_login_helper() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.json");
        for settings in [
            r#"{"sandbox":{"excludedCommands":["curl"]}}"#,
            r#"{"permissions":{"additionalDirectories":["/"]}}"#,
            r#"{"env":{"AWS_SECRET_ACCESS_KEY":"fake"}}"#,
        ] {
            std::fs::write(&path, settings).unwrap();
            assert!(check_claude_settings(root.path()).is_err());
        }
        std::fs::write(&path, r#"{"apiKeyHelper":"trusted-helper"}"#).unwrap();
        assert!(check_claude_settings(root.path()).is_ok());
    }
    #[test]
    fn codex_nested_protection_never_reopens_a_denied_parent() {
        let root = tempfile::tempdir().unwrap();
        let mut req = request(root.path());
        let secret = root.path().join("secrets ' \" $ \\ é");
        req.sandbox.deny_read.push(secret.clone());
        req.sandbox.deny_write.push(secret.join("nested"));
        let config = req
            .sandbox
            .codex_filesystem(&req, &root.path().join("auth"));
        let parsed: toml::Table = toml::from_str(&config).unwrap();
        let fs = &parsed["permissions"]["provefab"]["filesystem"];
        assert_eq!(fs[secret.to_str().unwrap()].as_str(), Some("deny"));
        assert!(
            fs.get(secret.join("nested").to_str().unwrap())
                .is_none_or(|v| v.as_str() == Some("deny"))
        );
        assert_eq!(fs[req.cwd.to_str().unwrap()].as_str(), Some("write"));
    }
    #[test]
    fn runtime_version_probe_rejects_old_unknown_and_failed_versions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("runtime");
        for body in ["echo 0.162.0", "echo unknown", "echo 0.162.1; exit 1"] {
            std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(check_version(&bin, [0, 162, 1]).is_err());
        }
        std::fs::write(&bin, "#!/bin/sh\necho 'codex-cli 0.162.1'\n").unwrap();
        assert!(check_version(&bin, [0, 162, 1]).is_ok());
    }
    #[test]
    fn explicit_parent_write_denials_cannot_be_overridden_by_workspace_or_tmp() {
        let root = tempfile::tempdir().unwrap();
        let mut req = request(root.path());
        req.sandbox.deny_write.push(root.path().into());
        assert!(
            prepare_request(&req, &[], &[]).is_err(),
            "private tmp would overlap a caller write denial"
        );
        let config = req
            .sandbox
            .codex_filesystem(&req, &root.path().join("auth"));
        let parsed: toml::Table = toml::from_str(&config).unwrap();
        assert_eq!(
            parsed["permissions"]["provefab"]["filesystem"][req.cwd.to_str().unwrap()].as_str(),
            Some("read")
        );
    }
    #[tokio::test]
    async fn sanitized_child_environment_drops_credentials_and_resets_git_config() {
        const CHILD: &str = "PROVEFAB_SANITIZER_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sandbox::tests::sanitized_child_environment_drops_credentials_and_resets_git_config", "--nocapture"])
                .env(CHILD, "1").env("FAKE_SECRET_TOKEN", "fake-do-not-leak")
                .env("AWS_SECRET_ACCESS_KEY", "fake-do-not-leak")
                .env("GIT_CONFIG_COUNT", "1").env("GIT_CONFIG_KEY_0", "credential.helper")
                .env("GIT_CONFIG_VALUE_0", "!printf fake-do-not-leak")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "printf '%s:%s:%s\\n' \"${FAKE_SECRET_TOKEN-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"$GIT_CONFIG_COUNT\"; git config --get credential.helper"]);
        sanitize_environment(&mut cmd);
        crate::apply_worker_env(
            &mut cmd,
            root.path(),
            &root.path().join("hooks"),
            ToolProfile::Full,
        );
        let result = cmd.output().await.unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&result.stdout), "unset:unset:3\n\n");
    }
    #[test]
    fn trusted_paths_and_runtime_cannot_live_in_writable_scratch() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let runtime = req.session_dir.join("sandbox-tmp/runtime");
        std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        std::fs::write(&runtime, "#!/bin/sh\necho 0.0.79\n").unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_request(&req, std::slice::from_ref(&runtime), &[]).is_err());
        let policy = SandboxPolicy {
            runtime,
            ..Default::default()
        };
        assert!(gate_command(&req.cwd, &req.session_dir, &policy, "touch marker").is_err());
    }
    #[test]
    fn writable_worktree_validation_symlink_is_rejected_before_canonicalization() {
        let root = tempfile::tempdir().unwrap();
        let mut req = request(root.path());
        let target = root.path().join("trusted-script");
        std::fs::write(&target, "exit 1").unwrap();
        let alias = req.cwd.join("validation-link.sh");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        req.sandbox.deny_write.push(alias);
        assert!(prepare_request(&req, &[], &[]).is_err());
    }
    #[test]
    fn writable_symlink_cannot_supply_an_external_trusted_config() {
        let root = tempfile::tempdir().unwrap();
        let req = request(root.path());
        let target = root.path().join("trusted");
        std::fs::write(&target, "trusted").unwrap();
        let alias = req.cwd.join("guard");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(prepare_request(&req, std::slice::from_ref(&alias), &[]).is_err());
    }
}
