use crate::{StageRequest, ToolProfile};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Explicit command restrictions. The requested localhost exception is currently unsupported.
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
pub(crate) use resolve_trusted_executable as executable;

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
    env.insert(
        "PATH".into(),
        trusted_path(&req.cwd, &req.session_dir)
            .to_string_lossy()
            .into(),
    );
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

/// Resolve the executable before changing cwd and reject writable-root aliases.
/// Callers must launch this returned path and apply `trusted_path` to PATH.
pub fn resolve_trusted_executable(
    program: &std::path::Path,
    worktree: &std::path::Path,
    scratch: &std::path::Path,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let worktree = canonical_path(worktree)?;
    let scratch = canonical_path(scratch)?;
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
    reject_worktree_symlink(&path, &worktree)?;
    reject_worktree_symlink(&path, &scratch.join("sandbox-tmp"))?;
    let path = path.canonicalize()?;
    disjoint(&worktree, &path)?;
    disjoint(&scratch.join("sandbox-tmp"), &path)?;
    if !path.is_file() || path.metadata()?.permissions().mode() & 0o111 == 0 {
        return Err(invalid("sandbox runtime is not executable"));
    }
    Ok(path)
}

// Even an external PATH directory can redirect an env-shebang interpreter through
// writable aliases. Inspect every entry; any ambiguity drops the whole directory.
fn trusted_path_target(
    path: &std::path::Path,
    worktree: &std::path::Path,
    temporary: &std::path::Path,
) -> Option<PathBuf> {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut pending: VecDeque<OsString> = absolute
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut prefix = PathBuf::from("/");
    let mut links = 0;
    while let Some(component) = pending.pop_front() {
        if component == "/" {
            prefix = PathBuf::from("/");
            continue;
        }
        if component == "." {
            continue;
        }
        if component == ".." {
            prefix.pop();
            continue;
        }
        let candidate = prefix.join(component);
        // Check every expanded prefix BEFORE following a link. Canonicalizing the
        // whole path first would erase a writable intermediate alias to a safe file.
        if candidate.starts_with(worktree) || candidate.starts_with(temporary) {
            return None;
        }
        let metadata = std::fs::symlink_metadata(&candidate).ok()?;
        if metadata.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return None;
            }
            let target = std::fs::read_link(&candidate).ok()?;
            for part in target.components().rev() {
                pending.push_front(part.as_os_str().to_owned());
            }
        } else {
            if !pending.is_empty() && !metadata.is_dir() {
                return None;
            }
            prefix = candidate;
        }
    }
    Some(prefix)
}

/// Resolve PATH before changing cwd; omit directories with writable or uninspectable entries.
/// An unusable path is explicit, never the empty entry that means current directory.
pub fn trusted_path(worktree: &std::path::Path, scratch: &std::path::Path) -> std::ffi::OsString {
    let (Ok(worktree), Ok(scratch)) = (canonical_path(worktree), canonical_path(scratch)) else {
        return "/dev/null".into();
    };
    let temporary = scratch.join("sandbox-tmp");
    let paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| !p.as_os_str().is_empty())
        .filter_map(|p| trusted_path_target(&p, &worktree, &temporary))
        .filter(|p| {
            if p.starts_with(&worktree) || p.starts_with(&temporary) {
                return false;
            }
            let Ok(mut entries) = std::fs::read_dir(p) else {
                return false;
            };
            entries.all(|entry| {
                entry.is_ok_and(|entry| {
                    trusted_path_target(&entry.path(), &worktree, &temporary).is_some()
                })
            })
        })
        .collect();
    if paths.is_empty() {
        return "/dev/null".into();
    }
    std::env::join_paths(paths).unwrap_or_else(|_| "/dev/null".into())
}

// Nonblocking output avoids deadlocking on a full pipe or a descendant retaining stdout.
fn probe_output(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn()?;
    let mut pipe = child.stdout.take().unwrap();
    let fd = pipe.as_raw_fd();
    // SAFETY: fd is owned by pipe and remains live throughout this function.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let deadline = std::time::Instant::now() + timeout;
    let result = (|| {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 8192];
            match pipe.read(&mut chunk) {
                Ok(0) => {
                    if let Some(status) = child.try_wait()? {
                        return if status.success() {
                            Ok(bytes)
                        } else {
                            Err(invalid("sandbox preflight command failed"))
                        };
                    }
                }
                Ok(n) => {
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.len() > 2_097_152 {
                        return Err(invalid("sandbox preflight output exceeded limit"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if std::time::Instant::now() >= deadline {
                return Err(invalid("sandbox preflight timed out"));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    })();
    // SAFETY: child starts a new process group. Kill descendants on success too.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.wait();
    result
}

pub(crate) fn check_version(
    program: &std::path::Path,
    minimum: [u64; 3],
    req: &StageRequest,
) -> std::io::Result<[u64; 3]> {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("--version")
        .env_clear()
        .envs(shell_environment())
        .env("PATH", trusted_path(&req.cwd, &req.session_dir));
    let output = probe_output(cmd, std::time::Duration::from_secs(5))?;
    let version = String::from_utf8_lossy(&output)
        .split_whitespace()
        .find_map(|v| {
            let values: Vec<u64> = v
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            <[u64; 3]>::try_from(values).ok()
        });
    version.filter(|v| *v >= minimum).ok_or_else(|| invalid(format!(
        "{} requires a verified native sandbox version >= {}.{}.{}; version probe failed or unsupported",
        program.display(), minimum[0], minimum[1], minimum[2]
    )))
}

fn personal_claude_account(bytes: &[u8]) -> std::io::Result<()> {
    let account: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid Claude auth status JSON"))?;
    if account["loggedIn"] != true
        || account["authMethod"] != "claude.ai"
        || account["apiProvider"] != "firstParty"
        || !matches!(account["subscriptionType"].as_str(), Some("pro" | "max"))
    {
        return Err(invalid(
            "Claude sandbox supports only personal firstParty claude.ai Pro/Max accounts; API, Team, Enterprise and unknown authentication are unsupported",
        ));
    }
    Ok(())
}

pub(crate) fn check_claude_account(
    program: &std::path::Path,
    config: &std::path::Path,
    req: &StageRequest,
) -> std::io::Result<()> {
    let mut cmd = tokio::process::Command::new(program);
    sanitize_environment(&mut cmd);
    apply_temporary_env(&mut cmd, req);
    cmd.env("CLAUDE_CONFIG_DIR", config)
        .env("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1")
        .args(["--setting-sources", "user", "auth", "status", "--json"])
        .current_dir(&req.session_dir);
    personal_claude_account(&probe_output(
        cmd.into_std(),
        std::time::Duration::from_secs(10),
    )?)
}

/// Gate commands use SRT. Local-network opt-in fails closed until loopback-only binding is verified.
/// Scratch must be trusted.
pub fn gate_command(
    worktree: &std::path::Path,
    scratch: &std::path::Path,
    policy: &SandboxPolicy,
    command: &str,
) -> std::io::Result<tokio::process::Command> {
    if policy.allow_local_network {
        return Err(invalid(
            "local network exception unavailable: SRT may bind non-loopback interfaces; loopback-only enforcement must be verified before enabling it",
        ));
    }
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
    check_version(&runtime, [0, 0, 79], &req)?;
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
    cmd.env("PATH", trusted_path(&req.cwd, &req.session_dir));
    for key in ["TMPDIR", "TMP", "TEMP", "CLAUDE_CODE_TMPDIR"] {
        cmd.env(key, req.session_dir.join("sandbox-tmp"));
    }
}

// Presence is enough: managed policy wins over --settings, even an empty cache can
// be populated later. Unknown v5 state is deliberately unsupported, not decoded.
fn reject_policy_source(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(invalid(format!(
            "cannot inspect Claude policy source {}: {e}",
            path.display()
        ))),
        Ok(_) => Err(invalid(format!(
            "unsupported Claude managed/cached policy source {}; use a dedicated unmanaged profile with no policy cache; do not delete authentication/state data. Use Codex or Pi while state support awaits verified effective-policy inspection",
            path.display()
        ))),
    }
}

/// Array merges and Edit allow rules can widen native sandbox permissions.
pub(crate) fn check_claude_settings(config_dir: &std::path::Path) -> std::io::Result<()> {
    for name in [
        "managed-settings.json",
        "managed-settings.d",
        "managed-mcp.json",
        "remote-settings.json",
        "remote-settings.json.signature.json",
        "remote-settings.json.signature-iat.json",
        "remote-settings-helper-consent",
        "remote-settings-consent.json",
        "state",
    ] {
        reject_policy_source(&config_dir.join(name))?;
    }
    let path = config_dir.join("settings.json");
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let settings: serde_json::Value = serde_json::from_slice(&data)
        .map_err(|e| invalid(format!("invalid Claude settings {}: {e}", path.display())))?;
    if settings.as_object().is_none_or(|object| !object.is_empty()) {
        return Err(invalid(format!(
            "Claude settings {} must be an empty object for the dedicated native-sandbox profile; inherited hooks, helpers, permissions and other overrides are unsupported. Keep login credentials separate; do not delete authentication data",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn check_claude_host_policy() -> std::io::Result<()> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| invalid("HOME required to inspect Claude policy state"))?;
    // The inspected binary protects both state roots; alternate storage semantics are
    // not verified. Refuse both rather than assume config_dir completely isolates it.
    check_claude_settings(&PathBuf::from(home).join(".claude"))?;
    for root in [
        "/Library/Application Support/ClaudeCode",
        "/etc/claude-code",
    ] {
        for name in [
            "managed-settings.json",
            "managed-settings.d",
            "managed-mcp.json",
        ] {
            reject_policy_source(&std::path::Path::new(root).join(name))?;
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Claude 2.1.296 Rxs/W2n read precisely these per-user/device plist paths.
        // Use OS identity, never inherited USER/LOGNAME; presence/unreadability refuses.
        let mut cmd = std::process::Command::new("/usr/bin/id");
        cmd.arg("-un").env_clear();
        let output = probe_output(cmd, std::time::Duration::from_secs(5))?;
        let user = std::str::from_utf8(&output)
            .map_err(|_| invalid("invalid OS account name"))?
            .trim();
        if user.is_empty() || user.contains(['/', '\0']) {
            return Err(invalid("invalid OS account name"));
        }
        for path in [
            PathBuf::from("/Library/Managed Preferences")
                .join(user)
                .join("com.anthropic.claudecode.plist"),
            PathBuf::from("/Library/Managed Preferences/com.anthropic.claudecode.plist"),
        ] {
            reject_policy_source(&path)?;
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
    fn claude_rejects_mergeable_security_settings_and_allows_empty_profile() {
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
        std::fs::write(&path, "{}").unwrap();
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
            assert!(check_version(&bin, [0, 162, 1], &request(root.path())).is_err());
        }
        std::fs::write(&bin, "#!/bin/sh\necho 'codex-cli 0.162.1'\n").unwrap();
        assert!(check_version(&bin, [0, 162, 1], &request(root.path())).is_ok());
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
    #[test]
    fn claude_rejects_inherited_edit_allow_rules() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("settings.json"),
            r#"{"permissions":{"allow":["Edit(//outside/**)"]}}"#,
        )
        .unwrap();
        assert!(check_claude_settings(root.path()).is_err());
    }

    #[test]
    fn claude_rejects_managed_cache_and_policy_helper() {
        for name in [
            "managed-settings.json",
            "remote-settings.json",
            "state",
            "remote-settings.json.signature.json",
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join(name), "{}").unwrap();
            assert!(
                check_claude_settings(root.path()).is_err(),
                "accepted {name}"
            );
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("settings.json"),
            r#"{"policyHelper":"echo unsafe"}"#,
        )
        .unwrap();
        assert!(check_claude_settings(root.path()).is_err());
    }

    #[test]
    fn gate_local_network_exception_is_refused_until_loopback_only_is_verified() {
        let root = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy {
            allow_local_network: true,
            runtime: "/bin/echo".into(),
            ..Default::default()
        };
        let error = gate_command(
            root.path(),
            &root.path().join("session"),
            &policy,
            "touch marker",
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-loopback"), "{error}");
        assert!(!root.path().join("marker").exists());
    }
    #[test]
    fn claude_personal_auth_is_fail_closed() {
        for subscription in ["pro", "max", "team", "enterprise", "unknown"] {
            let account = serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":subscription});
            assert_eq!(
                personal_claude_account(account.to_string().as_bytes()).is_ok(),
                matches!(subscription, "pro" | "max")
            );
        }
        for account in [
            serde_json::json!({}),
            serde_json::json!({"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","subscriptionType":"max"}),
            serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock","subscriptionType":"max"}),
            serde_json::json!({"loggedIn":false,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}),
        ] {
            assert!(personal_claude_account(account.to_string().as_bytes()).is_err());
        }
        assert!(personal_claude_account(b"invalid").is_err());
    }

    #[test]
    fn probes_bound_descendants_holding_stdout_open() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & exit 0"]);
        let started = std::time::Instant::now();
        assert!(
            probe_output(cmd, std::time::Duration::from_millis(100))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn claude_rejects_executable_user_settings_and_empty_state_directory() {
        for key in ["hooks", "apiKeyHelper", "statusLine", "policyHelper"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(
                root.path().join("settings.json"),
                serde_json::json!({key: "unsafe-command"}).to_string(),
            )
            .unwrap();
            assert!(check_claude_settings(root.path()).is_err());
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("state")).unwrap();
        assert!(check_claude_settings(root.path()).is_err());
        let broken = root.path().join("broken-cache");
        std::os::unix::fs::symlink(root.path().join("missing"), &broken).unwrap();
        assert!(reject_policy_source(&broken).is_err());
        let not_directory = root.path().join("file");
        std::fs::write(&not_directory, "").unwrap();
        assert!(reject_policy_source(&not_directory.join("policy")).is_err());
    }
    #[test]
    fn path_walker_resolves_relative_links_and_bounds_expansion() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        for name in ["bin", "safe", "work", "tmp"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("safe/tool"), "trusted").unwrap();
        std::os::unix::fs::symlink("../safe/tool", root.join("bin/tool")).unwrap();
        let resolve = |p: &Path| trusted_path_target(p, &root.join("work"), &root.join("tmp"));
        assert_eq!(
            resolve(&root.join("bin/tool")),
            Some(root.join("safe/tool"))
        );
        std::os::unix::fs::symlink("loop", root.join("bin/loop")).unwrap();
        assert!(resolve(&root.join("bin/loop")).is_none());
        std::os::unix::fs::symlink("../work/../safe/tool", root.join("bin/through-work")).unwrap();
        assert!(resolve(&root.join("bin/through-work")).is_none());
        for index in 0..41 {
            let target = if index == 40 {
                "../safe/tool".into()
            } else {
                format!("link{}", index + 1)
            };
            std::os::unix::fs::symlink(target, root.join(format!("bin/link{index}"))).unwrap();
        }
        assert!(resolve(&root.join("bin/link0")).is_none());
        assert_eq!(
            resolve(&root.join("bin/link1")),
            Some(root.join("safe/tool"))
        );
    }
}
