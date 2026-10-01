//! Issue trackers besides GitHub (docs/specs/2026-10-01-issue-trackers-design.md):
//! the `[repos.tracker]` table, credentials, and what Jira and Linear share.
//! Code and pull requests stay on GitHub; Provefab never changes a ticket's status.

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    #[default]
    Github,
    Jira,
    Linear,
}

impl TrackerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Github => "github",
            TrackerKind::Jira => "jira",
            TrackerKind::Linear => "linear",
        }
    }
}

/// `[repos.tracker]`: where the repository's issues live (spec §5). Absent
/// means GitHub. Unknown keys are refused, so a credential never loads from
/// `provefab.toml`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackerConfig {
    #[serde(default)]
    pub kind: TrackerKind,
    /// Jira only: the site's host name, `acme.atlassian.net`.
    #[serde(default)]
    pub site: Option<String>,
    /// The Jira project key or the Linear team key: `ENG` in `ENG-123`.
    #[serde(default)]
    pub project: Option<String>,
}

/// A bare host name: letters, digits, dots and hyphens, at least one dot, no
/// scheme, port or path.
pub fn is_host(s: &str) -> bool {
    !s.is_empty()
        && s.contains('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !s.starts_with(['.', '-'])
        && !s.ends_with(['.', '-'])
}

/// `[A-Z][A-Z0-9_]*`: the key part of `ENG-123`.
pub fn is_project_key(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Load-time rules of `[repos.tracker]` (spec §5).
pub fn validate(cfg: &TrackerConfig, label: &str) -> Result<(), String> {
    let kind = cfg.kind.as_str();
    match (cfg.kind, cfg.site.as_deref()) {
        (TrackerKind::Jira, None) => return Err("site is required for jira".into()),
        (TrackerKind::Jira, Some(site)) if !is_host(site) => {
            return Err(format!(
                "site `{site}` must be a host name such as acme.atlassian.net, without https:// or a path"
            ));
        }
        (TrackerKind::Github | TrackerKind::Linear, Some(_)) => {
            return Err(format!("site is for jira only, not {kind}"));
        }
        _ => {}
    }
    match (cfg.kind, cfg.project.as_deref()) {
        (TrackerKind::Github, Some(_)) => return Err("project is for jira and linear only".into()),
        (TrackerKind::Github, None) => {}
        (_, None) => return Err(format!("project is required for {kind}")),
        (_, Some(p)) if !is_project_key(p) => {
            return Err(format!("project `{p}` must look like ENG: [A-Z][A-Z0-9_]*"));
        }
        _ => {}
    }
    if cfg.kind == TrackerKind::Jira && label.chars().any(char::is_whitespace) {
        return Err(format!(
            "label `{label}` contains whitespace, which Jira labels cannot hold"
        ));
    }
    Ok(())
}

pub const JIRA_KEYCHAIN_SERVICE: &str = "provefab-jira";
pub const LINEAR_KEYCHAIN_SERVICE: &str = "provefab-linear";

/// Reads one environment variable; tests pass their own.
pub type Env<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);

/// The process environment, trimmed; an empty value counts as unset.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Jira Cloud Basic authentication: account e-mail and API token.
#[derive(Clone)]
pub struct JiraAuth {
    pub email: String,
    pub token: String,
}

impl std::fmt::Debug for JiraAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "JiraAuth {{ email: {:?}, token: <redacted> }}",
            self.email
        )
    }
}

/// `security` with `args`: its stdout (and stderr, for attribute listings) on
/// success. Never prompts.
async fn security_out(security: &Path, args: &[&str], with_stderr: bool) -> Option<String> {
    let out = Command::new(security)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if with_stderr {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Some(text)
}

/// The item comment in a `security find-generic-password` listing:
/// `    "icmt"<blob>="bot@acme.test"`.
fn keychain_comment(listing: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let v = l.trim().strip_prefix("\"icmt\"<blob>=")?;
        let v = v.strip_prefix('"')?.strip_suffix('"')?;
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// Jira credentials for `site`: `PROVEFAB_JIRA_EMAIL` and `PROVEFAB_JIRA_TOKEN`,
/// each overriding its part of the Keychain item `provefab-jira` / `<site>`
/// (spec §5). The error names the fix, never a secret.
pub async fn jira_auth(security: &Path, site: &str, env: Env<'_>) -> Result<JiraAuth, String> {
    let fix = format!(
        "run `provefab login jira --site {site}` or set PROVEFAB_JIRA_EMAIL and PROVEFAB_JIRA_TOKEN"
    );
    let token = match env("PROVEFAB_JIRA_TOKEN") {
        Some(t) => t,
        None => security_out(
            security,
            &[
                "find-generic-password",
                "-s",
                JIRA_KEYCHAIN_SERVICE,
                "-a",
                site,
                "-w",
            ],
            false,
        )
        .await
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("no Jira API token for {site}: {fix}"))?,
    };
    let email = match env("PROVEFAB_JIRA_EMAIL") {
        Some(e) => e,
        None => security_out(
            security,
            &[
                "find-generic-password",
                "-s",
                JIRA_KEYCHAIN_SERVICE,
                "-a",
                site,
            ],
            true,
        )
        .await
        .as_deref()
        .and_then(keychain_comment)
        .ok_or_else(|| format!("no Jira account e-mail for {site}: {fix}"))?,
    };
    Ok(JiraAuth { email, token })
}

/// The Linear personal API key: `PROVEFAB_LINEAR_KEY`, else the Keychain item
/// `provefab-linear` / `provefab` (spec §5).
pub async fn linear_key(security: &Path, env: Env<'_>) -> Result<String, String> {
    if let Some(k) = env("PROVEFAB_LINEAR_KEY") {
        return Ok(k);
    }
    security_out(
        security,
        &[
            "find-generic-password",
            "-s",
            LINEAR_KEYCHAIN_SERVICE,
            "-a",
            "provefab",
            "-w",
        ],
        false,
    )
    .await
    .map(|k| k.trim().to_string())
    .filter(|k| !k.is_empty())
    .ok_or_else(|| {
        "no Linear API key: run `provefab login linear` or set PROVEFAB_LINEAR_KEY".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_security(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn env_of(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<String> + Sync {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// `security` as macOS answers: the password alone with `-w`, the
    /// attributes (the comment holds the e-mail) without it.
    const KEYCHAIN: &str = r#"case "$*" in
  *"-s provefab-jira -a acme.atlassian.net -w") echo "kc-token" ;;
  *"-s provefab-jira -a acme.atlassian.net") echo 'keychain: "/Users/x/Library/Keychains/login.keychain-db"'; echo 'attributes:'; echo '    "acct"<blob>="acme.atlassian.net"'; echo '    "icmt"<blob>="bot@acme.test"' ;;
  *"-s provefab-linear -a provefab -w") echo "lin_api_kc" ;;
  *) exit 44 ;;
esac"#;

    #[test]
    fn hosts_and_project_keys() {
        for ok in ["acme.atlassian.net", "jira.acme-corp.io"] {
            assert!(is_host(ok), "{ok}");
        }
        for bad in [
            "",
            "acme",
            "https://acme.atlassian.net",
            "acme.atlassian.net/x",
            "-acme.net",
            "acme.net.",
            "ac me.net",
        ] {
            assert!(!is_host(bad), "{bad}");
        }
        for ok in ["ENG", "E", "ENG_2", "A1"] {
            assert!(is_project_key(ok), "{ok}");
        }
        for bad in ["", "eng", "1ENG", "EN-G", "EN G"] {
            assert!(!is_project_key(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn jira_credentials_come_from_the_environment_then_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let security = fake_security(dir.path(), "security", KEYCHAIN);
        let env = env_of(&[
            ("PROVEFAB_JIRA_EMAIL", "env@acme.test"),
            ("PROVEFAB_JIRA_TOKEN", "env-token"),
        ]);
        let a = jira_auth(&security, "acme.atlassian.net", &env)
            .await
            .unwrap();
        assert_eq!(
            (a.email.as_str(), a.token.as_str()),
            ("env@acme.test", "env-token")
        );
        let none = env_of(&[]);
        let a = jira_auth(&security, "acme.atlassian.net", &none)
            .await
            .unwrap();
        assert_eq!(
            (a.email.as_str(), a.token.as_str()),
            ("bot@acme.test", "kc-token")
        );
        assert!(!format!("{a:?}").contains("kc-token"), "{a:?}");
        let err = jira_auth(&security, "other.atlassian.net", &none)
            .await
            .unwrap_err();
        assert!(
            err.contains("provefab login jira --site other.atlassian.net"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_linear_key_comes_from_the_environment_then_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let security = fake_security(dir.path(), "security", KEYCHAIN);
        let env = env_of(&[("PROVEFAB_LINEAR_KEY", "lin_api_env")]);
        assert_eq!(linear_key(&security, &env).await.unwrap(), "lin_api_env");
        assert_eq!(
            linear_key(&security, &env_of(&[])).await.unwrap(),
            "lin_api_kc"
        );
        let missing = fake_security(dir.path(), "missing", "exit 44");
        let err = linear_key(&missing, &env_of(&[])).await.unwrap_err();
        assert!(err.contains("provefab login linear"), "{err}");
    }
}
