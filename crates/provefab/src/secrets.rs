//! Secrets Provefab keeps for itself (Linux design spec §3): the macOS
//! Keychain through `security`, or elsewhere `<home>/credentials.toml`,
//! readable by its owner only. Callers apply environment variables first.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// How long a Keychain read may take before it is abandoned.
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The secrets file in Provefab's home, on systems without the Keychain.
pub const FILE_NAME: &str = "credentials.toml";

/// Keychain services (`security ... -s <service>`), unchanged since 0.1.
pub const TYPESAFE_ITEM: &str = "provefab-typesafe";
pub const ANTHROPIC_ITEM: &str = "provefab-anthropic";
pub const JIRA_ITEM: &str = "provefab-jira";
pub const LINEAR_ITEM: &str = "provefab-linear";

/// A secret Provefab reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Name {
    /// The TypeSafe (Jev) key.
    Typesafe,
    /// The Anthropic API key of Claude Code models with `auth = "api_key"`.
    Anthropic,
    /// The Linear personal API key.
    Linear,
    /// The Jira API token of a site (lower-case host name).
    JiraToken(String),
    /// The Jira account e-mail of a site.
    JiraEmail(String),
}

impl Name {
    /// Keychain service and account; the TypeSafe key has always been read
    /// without an account.
    fn keychain(&self) -> (&'static str, Option<&str>) {
        match self {
            Name::Typesafe => (TYPESAFE_ITEM, None),
            Name::Anthropic => (ANTHROPIC_ITEM, Some("provefab")),
            Name::Linear => (LINEAR_ITEM, Some("provefab")),
            Name::JiraToken(site) | Name::JiraEmail(site) => (JIRA_ITEM, Some(site)),
        }
    }

    /// How a timeout names the Keychain item.
    fn item(&self) -> String {
        match self {
            Name::JiraToken(site) | Name::JiraEmail(site) => format!("{JIRA_ITEM} for {site}"),
            other => other.keychain().0.to_string(),
        }
    }

    /// Where it sits in the secrets file.
    fn keys(&self) -> Vec<&str> {
        match self {
            Name::Typesafe => vec!["typesafe"],
            Name::Anthropic => vec!["anthropic"],
            Name::Linear => vec!["linear"],
            Name::JiraToken(site) => vec!["jira", site, "token"],
            Name::JiraEmail(site) => vec!["jira", site, "email"],
        }
    }
}

/// What `provefab login` stores. No `Debug`: the e-mail is a credential (R3).
pub enum Entry {
    Typesafe,
    Anthropic,
    Linear,
    /// The token is asked for; the e-mail was asked for before, in clear.
    Jira {
        site: String,
        email: String,
    },
}

/// Fixed texts: never a value, never a line of the file (R3).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SecretError {
    #[error("timed out after {secs}s reading the Keychain item {item}")]
    TimedOut { item: String, secs: f64 },
    #[error("{path} can be read by group or others: run `chmod 600 {path}`")]
    OpenToOthers { path: String },
    #[error(
        "{path} is not valid TOML: fix it, or move it away and sign in again with `provefab login`"
    )]
    Malformed { path: String },
    #[error("{path}: {message}")]
    Io { path: String, message: String },
    #[error("could not store the credential in the Keychain")]
    KeychainWrite,
    #[error("nothing was entered; nothing was stored")]
    NothingEntered,
}

fn io_error(path: &Path, e: &std::io::Error) -> SecretError {
    SecretError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

/// Where Provefab's own secrets are (decision 1).
#[derive(Debug, Clone)]
pub enum Secrets {
    /// macOS: `security` with a time limit per read.
    Keychain {
        security: PathBuf,
        timeout: Duration,
    },
    /// Elsewhere: `<home>/credentials.toml`, mode 0600.
    File { path: PathBuf },
}

/// What a `security` call came back with.
enum Keychain {
    Found(String),
    Missing,
    TimedOut,
}

/// `security` with `args`: its stdout (and stderr, for attribute listings) on
/// success. A locked or unapproved Keychain item can make it prompt, so the
/// call is bounded by `limit`, stdin is closed and the child is killed when
/// the limit passes.
async fn security_out(
    security: &Path,
    args: &[&str],
    with_stderr: bool,
    limit: Duration,
) -> Keychain {
    let mut cmd = Command::new(security);
    cmd.args(args).stdin(Stdio::null()).kill_on_drop(true);
    let out = match tokio::time::timeout(limit, cmd.output()).await {
        Err(_) => return Keychain::TimedOut,
        Ok(Err(_)) => return Keychain::Missing,
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        return Keychain::Missing;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if with_stderr {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Keychain::Found(text)
}

/// The item comment in a `security find-generic-password` listing:
/// `    "icmt"<blob>="bot@acme.test"`. A non-ASCII comment is printed as hex
/// followed by a quoted rendering, `0x6A6F73C3A9  "jos\303\251"`; the hex
/// digits are decoded as UTF-8.
pub(crate) fn keychain_comment(listing: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let v = l.trim().strip_prefix("\"icmt\"<blob>=")?;
        if let Some(rest) = v.strip_prefix("0x") {
            let digits = rest.split_whitespace().next().unwrap_or("");
            return decode_hex_utf8(digits).filter(|s| !s.is_empty());
        }
        let v = v.strip_prefix('"')?.strip_suffix('"')?;
        (!v.is_empty()).then(|| v.to_string())
    })
}

fn decode_hex_utf8(digits: &str) -> Option<String> {
    if !digits.len().is_multiple_of(2) || !digits.is_ascii() {
        return None;
    }
    let bytes = (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

/// The file's table; `None` when there is no file. Refuses a mode that
/// grants anything to group or others before reading a byte (spec §3).
fn read_table(path: &Path) -> Result<Option<toml::Table>, SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path, &e)),
        Ok(m) => m,
    };
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(SecretError::OpenToOthers {
            path: path.display().to_string(),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| io_error(path, &e))?;
    toml::from_str::<toml::Table>(&text)
        .map(Some)
        .map_err(|_| SecretError::Malformed {
            path: path.display().to_string(),
        })
}

/// Temporary file in the same directory, created 0600, then renamed over
/// the file: a reader sees the old file or the new one, never a part.
fn write_atomic(path: &Path, text: &str) -> Result<(), SecretError> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir, &e))?;
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_error(path, &e)
    })
}

/// Echo off on a terminal until dropped (decision 8).
struct EchoOff {
    saved: libc::termios,
}

impl EchoOff {
    fn new() -> Option<Self> {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fills the struct when it returns 0; fd 0 is stdin.
        if unsafe { libc::tcgetattr(0, t.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: initialised by the successful tcgetattr above.
        let saved = unsafe { t.assume_init() };
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: a valid termios for fd 0, obtained from tcgetattr.
        (unsafe { libc::tcsetattr(0, libc::TCSANOW, &quiet) } == 0).then_some(Self { saved })
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in `new`.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved) };
    }
}

/// Whether stdin is a terminal (the masked read turns echo off only then).
pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 }
}

/// One line from `input`, trimmed, with echo off while a person types.
fn read_hidden(input: &mut dyn BufRead, tty: bool) -> Result<String, SecretError> {
    let echo = if tty { EchoOff::new() } else { None };
    let mut line = String::new();
    let read = input.read_line(&mut line);
    drop(echo);
    if tty {
        println!();
    }
    read.map_err(|e| io_error(Path::new("stdin"), &e))?;
    Ok(line.trim().to_string())
}

impl Secrets {
    /// The Keychain on macOS, the file in `home` elsewhere (spec §3).
    pub fn system(home: &Path) -> Self {
        if cfg!(target_os = "macos") {
            Self::keychain("security")
        } else {
            Self::file(home)
        }
    }

    pub fn keychain(security: impl Into<PathBuf>) -> Self {
        Self::Keychain {
            security: security.into(),
            timeout: KEYCHAIN_TIMEOUT,
        }
    }

    pub fn file(home: &Path) -> Self {
        Self::File {
            path: home.join(FILE_NAME),
        }
    }

    pub fn is_keychain(&self) -> bool {
        matches!(self, Self::Keychain { .. })
    }

    /// The stored value, trimmed; `None` when absent or empty.
    pub async fn get(&self, name: &Name) -> Result<Option<String>, SecretError> {
        match self {
            Self::Keychain { security, timeout } => {
                let (service, account) = name.keychain();
                let mut args = vec!["find-generic-password", "-s", service];
                if let Some(a) = account {
                    args.extend(["-a", a]);
                }
                let email = matches!(name, Name::JiraEmail(_));
                if !email {
                    args.push("-w");
                }
                match security_out(security, &args, email, *timeout).await {
                    Keychain::TimedOut => Err(SecretError::TimedOut {
                        item: name.item(),
                        secs: timeout.as_secs_f64(),
                    }),
                    Keychain::Missing => Ok(None),
                    Keychain::Found(text) if email => Ok(keychain_comment(&text)),
                    Keychain::Found(text) => {
                        Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty()))
                    }
                }
            }
            Self::File { path } => {
                let Some(table) = read_table(path)? else {
                    return Ok(None);
                };
                let keys = name.keys();
                let mut cur = table.get(keys[0]);
                for k in &keys[1..] {
                    cur = cur.and_then(|v| v.get(*k));
                }
                Ok(cur
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from))
            }
        }
    }

    /// Whether the secret is stored. On the Keychain only the attributes are
    /// read, never the password (as `doctor` always did).
    pub async fn has(&self, name: &Name) -> Result<bool, SecretError> {
        match self {
            Self::Keychain { security, timeout } => {
                let (service, account) = name.keychain();
                let mut args = vec!["find-generic-password", "-s", service];
                if let Some(a) = account {
                    args.extend(["-a", a]);
                }
                match security_out(security, &args, false, *timeout).await {
                    Keychain::TimedOut => Err(SecretError::TimedOut {
                        item: name.item(),
                        secs: timeout.as_secs_f64(),
                    }),
                    Keychain::Missing => Ok(false),
                    Keychain::Found(_) => Ok(true),
                }
            }
            Self::File { .. } => Ok(self.get(name).await?.is_some()),
        }
    }

    /// Where the secret is kept, for `doctor`: never its value.
    pub fn describe(&self, name: &Name) -> String {
        match self {
            Self::Keychain { .. } => format!("Keychain item `{}`", name.keychain().0),
            Self::File { path } => {
                let key = name
                    .keys()
                    .iter()
                    .map(|k| {
                        if k.contains('.') {
                            format!("\"{k}\"")
                        } else {
                            k.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                format!("`{key}` in {}", path.display())
            }
        }
    }

    /// `run` and `doctor` refuse a file open to others or that does not
    /// parse, even when no secret is read (spec §3).
    pub fn preflight(&self) -> Result<(), SecretError> {
        match self {
            Self::Keychain { .. } => Ok(()),
            Self::File { path } => read_table(path).map(|_| ()),
        }
    }

    /// Writes values into the file, keeping every other entry (decision 2).
    pub fn set_values(&self, values: &[(Name, &str)]) -> Result<(), SecretError> {
        let Self::File { path } = self else {
            return Err(SecretError::KeychainWrite);
        };
        let mut table = read_table(path)?.unwrap_or_default();
        for (name, value) in values {
            let keys = name.keys();
            let (last, parents) = keys.split_last().expect("every name has a key");
            let mut t = &mut table;
            for k in parents {
                if !t.get(*k).is_some_and(toml::Value::is_table) {
                    t.insert(k.to_string(), toml::Value::Table(toml::Table::new()));
                }
                t = t
                    .get_mut(*k)
                    .and_then(toml::Value::as_table_mut)
                    .expect("inserted above");
            }
            t.insert(last.to_string(), toml::Value::String(value.to_string()));
        }
        let text = toml::to_string(&table).map_err(|_| SecretError::Io {
            path: path.display().to_string(),
            message: "could not write the table".into(),
        })?;
        write_atomic(path, &text)
    }

    /// Asks for `what` and stores it (decision 2): at `security`'s own
    /// prompt on the Keychain, so the value never passes through Provefab;
    /// read from `input` with echo off on a terminal for the file.
    pub fn store(
        &self,
        entry: &Entry,
        what: &str,
        input: &mut dyn BufRead,
        tty: bool,
    ) -> Result<(), SecretError> {
        match self {
            Self::Keychain { security, .. } => {
                let (service, account, email) = match entry {
                    Entry::Typesafe => (TYPESAFE_ITEM, "provefab", None),
                    Entry::Anthropic => (ANTHROPIC_ITEM, "provefab", None),
                    Entry::Linear => (LINEAR_ITEM, "provefab", None),
                    Entry::Jira { site, email } => (JIRA_ITEM, site.as_str(), Some(email.as_str())),
                };
                let mut args = vec!["add-generic-password", "-U", "-s", service, "-a", account];
                if let Some(e) = email {
                    args.extend(["-j", e]);
                }
                args.push("-w");
                println!("Enter {what} at the Keychain prompt.");
                let status = std::process::Command::new(security)
                    .args(&args)
                    .status()
                    .map_err(|_| SecretError::KeychainWrite)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(SecretError::KeychainWrite)
                }
            }
            Self::File { .. } => {
                print!("Paste {what} (not shown), then press Enter: ");
                let _ = std::io::stdout().flush();
                let value = read_hidden(input, tty)?;
                if value.is_empty() {
                    return Err(SecretError::NothingEntered);
                }
                match entry {
                    Entry::Typesafe => self.set_values(&[(Name::Typesafe, &value)]),
                    Entry::Anthropic => self.set_values(&[(Name::Anthropic, &value)]),
                    Entry::Linear => self.set_values(&[(Name::Linear, &value)]),
                    Entry::Jira { site, email } => self.set_values(&[
                        (Name::JiraToken(site.clone()), &value),
                        (Name::JiraEmail(site.clone()), email),
                    ]),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;

    fn fake(dir: &Path, name: &str, script: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    const SECRET: &str = "sk-ant-SENTINEL-4242";

    #[tokio::test]
    async fn the_file_is_created_owner_only_and_keeps_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::file(dir.path());
        let Secrets::File { path } = &s else { panic!() };
        assert_eq!(s.get(&Name::Anthropic).await.unwrap(), None);
        s.set_values(&[(Name::Anthropic, SECRET)]).unwrap();
        assert_eq!(mode(path), 0o600);
        s.set_values(&[(Name::Linear, "lin_api_1")]).unwrap();
        s.set_values(&[
            (Name::JiraToken("acme.atlassian.net".into()), "jira-tok"),
            (
                Name::JiraEmail("acme.atlassian.net".into()),
                "bot@acme.test",
            ),
        ])
        .unwrap();
        assert_eq!(mode(path), 0o600);
        assert_eq!(
            s.get(&Name::Anthropic).await.unwrap().as_deref(),
            Some(SECRET)
        );
        assert_eq!(
            s.get(&Name::Linear).await.unwrap().as_deref(),
            Some("lin_api_1")
        );
        assert_eq!(
            s.get(&Name::JiraEmail("acme.atlassian.net".into()))
                .await
                .unwrap()
                .as_deref(),
            Some("bot@acme.test")
        );
        assert_eq!(
            s.get(&Name::JiraToken("acme.atlassian.net".into()))
                .await
                .unwrap()
                .as_deref(),
            Some("jira-tok")
        );
        assert_eq!(
            s.get(&Name::JiraToken("other.atlassian.net".into()))
                .await
                .unwrap(),
            None
        );
        assert_eq!(s.get(&Name::Typesafe).await.unwrap(), None);
        // The Jira fields are a table per site, as spec §3 names them.
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("[jira.\"acme.atlassian.net\"]"), "{text}");
        // No temporary file is left beside it.
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![FILE_NAME.to_string()]);
    }

    #[tokio::test]
    async fn a_file_open_to_others_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::file(dir.path());
        s.set_values(&[(Name::Anthropic, SECRET)]).unwrap();
        let path = dir.path().join(FILE_NAME);
        for wide in [0o644, 0o640, 0o604, 0o620, 0o602, 0o610, 0o601] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(wide)).unwrap();
            let want = SecretError::OpenToOthers {
                path: path.display().to_string(),
            };
            assert_eq!(s.get(&Name::Anthropic).await, Err(want.clone()), "{wide:o}");
            assert_eq!(s.preflight(), Err(want.clone()), "{wide:o}");
            assert_eq!(s.set_values(&[(Name::Linear, "x")]), Err(want), "{wide:o}");
        }
        let err = s.get(&Name::Anthropic).await.unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "{} can be read by group or others: run `chmod 600 {}`",
                path.display(),
                path.display()
            )
        );
        assert!(!err.contains(SECRET));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(s.preflight(), Ok(()));
        assert!(
            s.get(&Name::Linear).await.unwrap().is_none(),
            "the refused write changed nothing"
        );
        // No file at all is fine.
        assert_eq!(
            Secrets::file(&dir.path().join("nothing")).preflight(),
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_malformed_file_never_shows_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, format!("anthropic = \"{SECRET}\nlinear = 1 2\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = Secrets::file(dir.path());
        let err = s.get(&Name::Anthropic).await.unwrap_err();
        assert_eq!(
            err,
            SecretError::Malformed {
                path: path.display().to_string()
            }
        );
        for shown in [err.to_string(), format!("{err:?}"), format!("{s:?}")] {
            assert!(!shown.contains(SECRET), "{shown}");
        }
        assert!(s.preflight().is_err());
    }

    #[tokio::test]
    async fn an_empty_or_non_string_value_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "anthropic = \"  \"\nlinear = 3\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = Secrets::file(dir.path());
        assert_eq!(s.get(&Name::Anthropic).await.unwrap(), None);
        assert_eq!(s.get(&Name::Linear).await.unwrap(), None);
        assert!(!s.has(&Name::Linear).await.unwrap());
    }

    #[tokio::test]
    async fn the_file_backend_stores_what_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::file(dir.path());
        s.store(
            &Entry::Linear,
            "a Linear key",
            &mut Cursor::new(b"  lin_api_typed \n".to_vec()),
            false,
        )
        .unwrap();
        assert_eq!(
            s.get(&Name::Linear).await.unwrap().as_deref(),
            Some("lin_api_typed")
        );
        let jira = Entry::Jira {
            site: "acme.atlassian.net".into(),
            email: "bot@acme.test".into(),
        };
        s.store(
            &jira,
            "a Jira token",
            &mut Cursor::new(b"tok\n".to_vec()),
            false,
        )
        .unwrap();
        assert_eq!(
            s.get(&Name::JiraEmail("acme.atlassian.net".into()))
                .await
                .unwrap()
                .as_deref(),
            Some("bot@acme.test")
        );
        assert_eq!(
            s.store(
                &Entry::Typesafe,
                "a key",
                &mut Cursor::new(b"\n".to_vec()),
                false
            ),
            Err(SecretError::NothingEntered)
        );
        assert_eq!(s.get(&Name::Typesafe).await.unwrap(), None);
    }

    /// `security` as macOS answers: the password alone with `-w`, the
    /// attributes (the comment holds the e-mail) without it.
    const KEYCHAIN: &str = r#"case "$*" in
  *"-s provefab-jira -a acme.atlassian.net -w") echo "kc-token" ;;
  *"-s provefab-jira -a acme.atlassian.net") echo 'keychain: "/Users/x/Library/Keychains/login.keychain-db"'; echo 'attributes:'; echo '    "acct"<blob>="acme.atlassian.net"'; echo '    "icmt"<blob>="bot@acme.test"' ;;
  *"-s provefab-linear -a provefab -w") echo "lin_api_kc" ;;
  *"-s provefab-typesafe -w") echo "ts_kc" ;;
  *"-s provefab-anthropic -a provefab") echo 'attributes:' ;;
  *) exit 44 ;;
esac"#;

    #[tokio::test]
    async fn the_keychain_backend_reads_the_same_items_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::keychain(fake(dir.path(), "security", KEYCHAIN));
        let site = || "acme.atlassian.net".to_string();
        assert_eq!(
            s.get(&Name::JiraToken(site())).await.unwrap().as_deref(),
            Some("kc-token")
        );
        assert_eq!(
            s.get(&Name::JiraEmail(site())).await.unwrap().as_deref(),
            Some("bot@acme.test")
        );
        assert_eq!(
            s.get(&Name::Linear).await.unwrap().as_deref(),
            Some("lin_api_kc")
        );
        assert_eq!(
            s.get(&Name::Typesafe).await.unwrap().as_deref(),
            Some("ts_kc")
        );
        assert_eq!(
            s.get(&Name::JiraToken("other.atlassian.net".into()))
                .await
                .unwrap(),
            None
        );
        // `has` asks for the attributes only, never the password.
        assert!(s.has(&Name::Anthropic).await.unwrap());
        assert!(!s.has(&Name::Linear).await.unwrap());
        assert_eq!(
            s.describe(&Name::Anthropic),
            "Keychain item `provefab-anthropic`"
        );
        assert_eq!(s.preflight(), Ok(()));
    }

    #[tokio::test]
    async fn a_keychain_read_times_out_with_fixed_text() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::Keychain {
            security: fake(dir.path(), "slow", "sleep 30"),
            timeout: Duration::from_millis(200),
        };
        let started = std::time::Instant::now();
        let err = s
            .get(&Name::JiraToken("acme.atlassian.net".into()))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "timed out after 0.2s reading the Keychain item provefab-jira for acme.atlassian.net"
        );
        let err = s.get(&Name::Linear).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "timed out after 0.2s reading the Keychain item provefab-linear"
        );
        assert!(s.has(&Name::Anthropic).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_keychain_store_lets_security_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls.txt");
        let ok = fake(
            dir.path(),
            "security",
            &format!("echo \"$@\" >> {}", calls.display()),
        );
        let s = Secrets::keychain(ok);
        let jira = Entry::Jira {
            site: "acme.atlassian.net".into(),
            email: "bot@acme.test".into(),
        };
        let mut nothing = Cursor::new(Vec::new());
        for e in [Entry::Typesafe, Entry::Anthropic, Entry::Linear, jira] {
            s.store(&e, "it", &mut nothing, false).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "add-generic-password -U -s provefab-typesafe -a provefab -w\n\
             add-generic-password -U -s provefab-anthropic -a provefab -w\n\
             add-generic-password -U -s provefab-linear -a provefab -w\n\
             add-generic-password -U -s provefab-jira -a acme.atlassian.net -j bot@acme.test -w\n"
        );
        let refused = Secrets::keychain(fake(dir.path(), "no", "exit 1"));
        assert_eq!(
            refused.store(&Entry::Linear, "it", &mut nothing, false),
            Err(SecretError::KeychainWrite)
        );
        assert_eq!(
            refused.set_values(&[(Name::Linear, "x")]),
            Err(SecretError::KeychainWrite)
        );
    }

    #[test]
    fn keychain_comment_decodes_hex_utf8() {
        let hex = |h: &str| format!("    \"icmt\"<blob>=0x{h}  \"jos\\303\\251@acme.test\"");
        assert_eq!(
            keychain_comment(&hex("6A6F73C3A94061636D652E74657374")).as_deref(),
            Some("josé@acme.test")
        );
        assert_eq!(
            keychain_comment(&hex("6a6f73c3a94061636d652e74657374")).as_deref(),
            Some("josé@acme.test")
        );
        // Odd length, invalid UTF-8, no digits.
        assert_eq!(keychain_comment(&hex("6A6F7")), None);
        assert_eq!(keychain_comment(&hex("C328")), None);
        assert_eq!(keychain_comment(&hex("")), None);
        assert_eq!(keychain_comment("    \"icmt\"<blob>=<NULL>"), None);
    }

    #[test]
    fn descriptions_name_the_store_and_never_a_value() {
        let s = Secrets::file(Path::new("/h/.provefab"));
        assert_eq!(
            s.describe(&Name::JiraToken("acme.atlassian.net".into())),
            "`jira.\"acme.atlassian.net\".token` in /h/.provefab/credentials.toml"
        );
        assert_eq!(
            s.describe(&Name::Anthropic),
            "`anthropic` in /h/.provefab/credentials.toml"
        );
        assert!(!s.is_keychain());
        assert!(Secrets::keychain("security").is_keychain());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keeps_the_keychain() {
        assert!(Secrets::system(Path::new("/h")).is_keychain());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_systems_use_the_file_in_provefab_home() {
        let Secrets::File { path } = Secrets::system(Path::new("/h/.provefab")) else {
            panic!("expected the file backend")
        };
        assert_eq!(path, PathBuf::from("/h/.provefab/credentials.toml"));
    }
}
