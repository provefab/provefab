//! Issue trackers besides GitHub (docs/specs/2026-10-01-issue-trackers-design.md):
//! the `[repos.tracker]` table, credentials, and what Jira and Linear share.
//! Code and pull requests stay on GitHub; Provefab never changes a ticket's status.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use crate::config::Config;
use crate::forge::{Comment, ForgeError, Gh, Issue, PrStatus};
use crate::jira::Jira;
use crate::linear::Linear;
use crate::ports::{Forge, Tracker};

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

impl JiraAuth {
    /// The value after `Basic ` in the Authorization header: base64 of
    /// `email:token`. A secret too, so errors redact it.
    pub fn basic(&self) -> String {
        base64(format!("{}:{}", self.email, self.token).as_bytes())
    }
}

/// Neither part is shown: the e-mail names the account the token unlocks.
impl std::fmt::Debug for JiraAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JiraAuth { email: <redacted>, token: <redacted> }")
    }
}

/// Standard base64 with padding (RFC 4648), for [`JiraAuth::basic`] only.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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

/// How Provefab names an issue: `#123` on GitHub, its key (`ENG-123`) on a tracker.
pub fn issue_ref(number: u64, key: Option<&str>) -> String {
    match key {
        Some(k) => k.to_string(),
        None => format!("#{number}"),
    }
}

/// An issue with its repository: `o/r#123`, or `o/r ENG-123`.
pub fn repo_ref(slug: &str, number: u64, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("{slug} {k}"),
        None => format!("{slug}#{number}"),
    }
}

/// The pull request title: the issue title, prefixed with a ticket's key so
/// the Jira and Linear GitHub integrations link it (spec §4).
pub fn pr_title(title: &str, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("{k}: {title}"),
        None => title.to_string(),
    }
}

/// The pull request body's first line (spec §4). `Fixes ENG-123` lets Linear's
/// own GitHub integration close the ticket on merge if the team enabled it;
/// Provefab never changes a status itself.
pub fn pr_first_line(kind: TrackerKind, number: u64, key: Option<&str>, url: &str) -> String {
    match (key, kind) {
        (None, _) => format!("Closes #{number}."),
        (Some(k), TrackerKind::Linear) => format!("Issue: [{k}]({url}). Fixes {k}"),
        (Some(k), _) => format!("Issue: [{k}]({url})."),
    }
}

/// A timestamp as Provefab stores and compares them: UTC, whole seconds,
/// `YYYY-MM-DDTHH:MM:SSZ` (spec §6, §7). Accepts `Z`, `+HH:MM` and `+HHMM`
/// offsets and fractional seconds; `None` for anything else.
pub fn utc_seconds(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            sign * (digits[..2].parse::<i64>().ok()? * 3600 + digits[2..].parse::<i64>().ok()? * 60)
        }
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset;
    Some(crate::store::rfc3339(secs))
}

/// Howard Hinnant's days-from-civil: the inverse of `store::rfc3339`'s.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The number of `key` when it belongs to `project`: `("ENG-123", "ENG")` gives 123.
pub fn number_of(key: &str, project: &str) -> Option<u64> {
    key.strip_prefix(project)?.strip_prefix('-')?.parse().ok()
}

/// Budget of one Jira or Linear call.
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// A 429 whose `Retry-After` is at most this many seconds is waited out once
/// inside the call (plan decision 9); a longer one is a transient error.
pub const SHORT_RETRY: u64 = 30;

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// `text` with every credential replaced: an API may echo what it was sent.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|s| s.len() >= 4)
        .fold(text.to_string(), |t, s| t.replace(s, "<redacted>"))
}

/// Sends the request `make` builds (built again for the one retry) and returns
/// status, `Retry-After` seconds and body. Only a failure to get an answer is
/// an error here; the caller decides what a status means.
pub async fn send(
    service: &'static str,
    secrets: &[&str],
    make: impl Fn() -> reqwest::RequestBuilder,
) -> Result<(u16, Option<u64>, String), ForgeError> {
    let mut waited = false;
    loop {
        let resp = make()
            .send()
            .await
            .map_err(|e| network_error(service, &e, secrets))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp
            .text()
            .await
            .map_err(|e| network_error(service, &e, secrets))?;
        match retry_after {
            Some(secs) if status == 429 && !waited && secs <= SHORT_RETRY => {
                waited = true;
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            _ => return Ok((status, retry_after, body)),
        }
    }
}

fn network_error(service: &'static str, e: &reqwest::Error, secrets: &[&str]) -> ForgeError {
    ForgeError::Tracker {
        service,
        status: None,
        message: redact(&e.to_string(), secrets),
    }
}

/// A status that is not a success, with what the service said about it (Jira
/// `errorMessages` and `errors`, GraphQL `errors[].message`), redacted.
pub fn http_error(
    service: &'static str,
    status: u16,
    retry_after: Option<u64>,
    body: &str,
    secrets: &[&str],
) -> ForgeError {
    let what = match status {
        401 => "the credentials were refused",
        403 => "no access to this project or ticket",
        404 => "project or ticket not found",
        429 => "rate limited",
        500..=599 => "server error",
        _ => "request refused",
    };
    let mut message = what.to_string();
    // Redacted before it is cut, so a cut never leaves part of a secret.
    let said: String = redact(&server_messages(body), secrets)
        .chars()
        .take(300)
        .collect();
    if !said.is_empty() {
        message.push_str(": ");
        message.push_str(&said);
    }
    if let Some(secs) = retry_after {
        message.push_str(&format!(" (retry after {secs} s)"));
    }
    ForgeError::Tracker {
        service,
        status: Some(status),
        message: redact(&message, secrets),
    }
}

fn server_messages(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    out.extend(
        v["errorMessages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str().map(str::to_string)),
    );
    if let Some(fields) = v["errors"].as_object() {
        out.extend(
            fields
                .iter()
                .filter_map(|(k, m)| m.as_str().map(|m| format!("{k}: {m}"))),
        );
    }
    out.extend(
        v["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["message"].as_str().map(str::to_string)),
    );
    out.join("; ")
}

/// A ticket link `provefab add` accepts (spec §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketUrl {
    /// `https://<site>/browse/ENG-123`
    Jira { site: String, key: String },
    /// `https://linear.app/<workspace>/issue/ENG-123[/...]`
    Linear { workspace: String, key: String },
}

/// `ENG-123` as (`ENG`, 123); the number is never 0.
pub fn split_key(key: &str) -> Option<(&str, u64)> {
    let (project, n) = key.rsplit_once('-')?;
    let n: u64 = n.parse().ok()?;
    (is_project_key(project) && n > 0).then_some((project, n))
}

/// A Jira or Linear ticket link, its key upper case and a Jira site lower
/// case; the query string and fragment are ignored.
pub fn parse_ticket_url(url: &str) -> Option<TicketUrl> {
    let url = url.trim();
    let url = url.split(['?', '#']).next()?.trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    let key = |k: &str| {
        let k = k.to_uppercase();
        split_key(&k).is_some().then_some(k)
    };
    match parts.as_slice() {
        ["linear.app", workspace, "issue", k, ..] if !workspace.is_empty() => {
            Some(TicketUrl::Linear {
                workspace: workspace.to_string(),
                key: key(k)?,
            })
        }
        [site, "browse", k] if is_host(site) => Some(TicketUrl::Jira {
            site: site.to_lowercase(),
            key: key(k)?,
        }),
        _ => None,
    }
}

/// A repository's tracker when it is not GitHub.
pub enum Remote {
    Jira(Jira),
    Linear(Linear),
}

impl Remote {
    /// The adapter for a Jira or Linear `[repos.tracker]`, with its
    /// credentials; `None` for GitHub. A missing credential is an error naming
    /// the `provefab login` to run. Shared by `provefab run` and `doctor`.
    pub async fn from_tracker(
        t: &TrackerConfig,
        security: &Path,
        env: Env<'_>,
    ) -> Result<Option<Self>, String> {
        let project = t.project.as_deref().unwrap_or_default();
        Ok(Some(match t.kind {
            TrackerKind::Github => return Ok(None),
            TrackerKind::Jira => {
                let site = t.site.as_deref().unwrap_or_default();
                let auth = jira_auth(security, site, env).await?;
                Remote::Jira(Jira::new(&format!("https://{site}"), site, project, auth))
            }
            TrackerKind::Linear => Remote::Linear(Linear::new(
                crate::linear::API,
                project,
                linear_key(security, env).await?,
            )),
        }))
    }

    /// For `provefab doctor`: the credentials authenticate and the project or
    /// team is readable.
    pub async fn check(&self) -> Result<String, ForgeError> {
        match self {
            Remote::Jira(j) => j.check().await,
            Remote::Linear(l) => l.check().await,
        }
    }
}

/// The production hub (spec §3): each repository's issues go to its tracker,
/// found by slug (so stored pending effects replay to the right one), and
/// every forge call goes to GitHub.
pub struct Routed {
    pub gh: Gh,
    /// By lower-case slug; repositories without an entry use GitHub issues.
    pub remotes: HashMap<String, Remote>,
}

impl Routed {
    pub fn remote(&self, slug: &str) -> Option<&Remote> {
        self.remotes.get(&slug.to_lowercase())
    }

    /// One adapter per Jira or Linear repository, with its credentials. A
    /// missing credential is an error naming the `provefab login` to run.
    pub async fn from_config(
        config: &Config,
        gh: Gh,
        security: &Path,
        env: Env<'_>,
    ) -> Result<Self, String> {
        let mut remotes = HashMap::new();
        for repo in &config.repos {
            let Some(t) = &repo.tracker else { continue };
            if let Some(remote) = Remote::from_tracker(t, security, env).await? {
                remotes.insert(repo.slug.to_lowercase(), remote);
            }
        }
        Ok(Self { gh, remotes })
    }
}

/// Calls `method` on the slug's tracker, or on `Gh` for a GitHub repository.
macro_rules! route {
    ($self:ident, $slug:ident, $method:ident($($arg:expr),*)) => {
        match $self.remote($slug) {
            Some(Remote::Jira(j)) => Tracker::$method(j, $slug, $($arg),*).await,
            Some(Remote::Linear(l)) => Tracker::$method(l, $slug, $($arg),*).await,
            None => Tracker::$method(&$self.gh, $slug, $($arg),*).await,
        }
    };
}

impl Tracker for Routed {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        route!(self, slug, open_issues(label))
    }
    async fn issue(&self, slug: &str, number: u64) -> Result<Issue, ForgeError> {
        route!(self, slug, issue(number))
    }
    async fn comments(&self, slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        route!(self, slug, comments(number))
    }
    async fn comment(&self, slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        route!(self, slug, comment(number, body))
    }
    async fn edit_labels(
        &self,
        slug: &str,
        number: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), ForgeError> {
        route!(self, slug, edit_labels(number, add, remove))
    }
    async fn ensure_label(
        &self,
        slug: &str,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), ForgeError> {
        route!(self, slug, ensure_label(name, color, description))
    }
    async fn issue_open(&self, slug: &str, number: u64) -> Result<bool, ForgeError> {
        route!(self, slug, issue_open(number))
    }
}

impl Forge for Routed {
    async fn pr_comment(&self, slug: &str, url: &str, body: &str) -> Result<(), ForgeError> {
        Forge::pr_comment(&self.gh, slug, url, body).await
    }
    async fn pr_create(
        &self,
        slug: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        Forge::pr_create(&self.gh, slug, head, base, title, body).await
    }
    async fn repo_clone(&self, slug: &str, dest: &Path) -> Result<(), ForgeError> {
        Forge::repo_clone(&self.gh, slug, dest).await
    }
    async fn pr_status(&self, slug: &str, url: &str) -> Result<PrStatus, ForgeError> {
        Forge::pr_status(&self.gh, slug, url).await
    }
    async fn pr_merge(&self, slug: &str, url: &str, head: &str) -> Result<(), ForgeError> {
        Forge::pr_merge(&self.gh, slug, url, head).await
    }
    async fn repo_is_public(&self, slug: &str) -> Result<bool, ForgeError> {
        Forge::repo_is_public(&self.gh, slug).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::forge::Gh;
    use crate::jira::Jira;
    use crate::ports::{Forge, Tracker};
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    #[test]
    fn references_titles_and_first_lines() {
        assert_eq!(issue_ref(7, None), "#7");
        assert_eq!(issue_ref(7, Some("ENG-7")), "ENG-7");
        assert_eq!(repo_ref("o/r", 7, None), "o/r#7");
        assert_eq!(repo_ref("o/r", 7, Some("ENG-7")), "o/r ENG-7");
        assert_eq!(pr_title("Fix it", None), "Fix it");
        assert_eq!(pr_title("Fix it", Some("ENG-7")), "ENG-7: Fix it");
        let gh = "https://github.com/o/r/issues/7";
        assert_eq!(
            pr_first_line(TrackerKind::Github, 7, None, gh),
            "Closes #7."
        );
        let jira = "https://acme.atlassian.net/browse/ENG-7";
        assert_eq!(
            pr_first_line(TrackerKind::Jira, 7, Some("ENG-7"), jira),
            "Issue: [ENG-7](https://acme.atlassian.net/browse/ENG-7)."
        );
        let linear = "https://linear.app/acme/issue/ENG-7/fix-it";
        assert_eq!(
            pr_first_line(TrackerKind::Linear, 7, Some("ENG-7"), linear),
            "Issue: [ENG-7](https://linear.app/acme/issue/ENG-7/fix-it). Fixes ENG-7"
        );
        // A task queued from GitHub before the repository moved keeps its closing line.
        assert_eq!(pr_first_line(TrackerKind::Jira, 7, None, gh), "Closes #7.");
    }
    #[test]
    fn utc_seconds_normalises_offsets_fractions_and_dates() {
        for (raw, utc) in [
            ("2026-10-01T10:00:00.000+0200", "2026-10-01T08:00:00Z"),
            ("2026-10-01T01:30:00.123+02:00", "2026-09-30T23:30:00Z"),
            ("2026-10-01T08:00:00.123Z", "2026-10-01T08:00:00Z"),
            ("2026-10-01T08:00:00Z", "2026-10-01T08:00:00Z"),
            ("2024-02-29T23:00:00-0130", "2024-03-01T00:30:00Z"),
            ("2026-12-31T23:59:59.999-0100", "2027-01-01T00:59:59Z"),
        ] {
            assert_eq!(utc_seconds(raw).as_deref(), Some(utc), "{raw}");
        }
        for bad in [
            "",
            "2026-10-01",
            "garbage",
            "2026-13-01T00:00:00Z",
            "2026-10-01T08:00:00",
            "2026-10-01T08:00:00+2",
        ] {
            assert_eq!(utc_seconds(bad), None, "{bad}");
        }
        // Provefab's own timestamps are a fixed point, so string order is time order.
        for secs in [0, 951_782_400, 1_790_000_000] {
            let s = crate::store::rfc3339(secs);
            assert_eq!(utc_seconds(&s), Some(s.clone()));
        }
    }

    #[test]
    fn numbers_come_from_keys_of_the_project() {
        assert_eq!(number_of("ENG-123", "ENG"), Some(123));
        assert_eq!(number_of("OPS-123", "ENG"), None);
        assert_eq!(number_of("ENG-x", "ENG"), None);
        assert_eq!(number_of("ENGX-1", "ENG"), None);
    }

    #[test]
    fn http_errors_are_classified_and_redacted() {
        let secrets = ["tok-SECRET-123", "bot@acme.test"];
        let body = r#"{"errorMessages":["no access for bot@acme.test with tok-SECRET-123"]}"#;
        for (status, permanent) in [
            (400, true),
            (401, true),
            (403, true),
            (404, true),
            (408, false),
            (429, false),
            (500, false),
            (503, false),
        ] {
            let e = http_error("jira", status, None, body, &secrets);
            assert_eq!(e.is_permanent(), permanent, "{status}");
            let shown = format!("{e} {e:?}");
            assert!(
                !shown.contains("tok-SECRET-123") && !shown.contains("bot@acme.test"),
                "{shown}"
            );
            assert!(shown.contains(&format!("HTTP {status}")), "{shown}");
        }
        let e = http_error("jira", 429, Some(120), "", &secrets);
        assert!(e.to_string().contains("retry after 120 s"), "{e}");
    }

    #[test]
    fn secrets_are_redacted_before_the_message_is_cut() {
        let secret = "tok-SECRET-123";
        let body = json!({"errorMessages": [format!("{}{secret}", "x".repeat(295))]}).to_string();
        let shown = http_error("jira", 400, None, &body, &[secret]).to_string();
        for n in 3..=secret.len() {
            assert!(!shown.contains(&secret[..n]), "{n}: {shown}");
        }
    }

    #[test]
    fn jira_auth_hides_both_parts_and_its_basic_value_is_the_header() {
        let a = JiraAuth {
            email: "bot@acme.test".into(),
            token: "tok-SECRET-123".into(),
        };
        let shown = format!("{a:?}");
        assert!(
            !shown.contains("bot@acme.test") && !shown.contains("tok-SECRET-123"),
            "{shown}"
        );
        assert_eq!(a.basic(), "Ym90QGFjbWUudGVzdDp0b2stU0VDUkVULTEyMw==");
        for (raw, b64) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
        ] {
            assert_eq!(base64(raw.as_bytes()), b64, "{raw}");
        }
    }

    #[test]
    fn ticket_urls() {
        let jira = |site: &str, key: &str| {
            Some(TicketUrl::Jira {
                site: site.into(),
                key: key.into(),
            })
        };
        let linear = |ws: &str, key: &str| {
            Some(TicketUrl::Linear {
                workspace: ws.into(),
                key: key.into(),
            })
        };
        assert_eq!(
            parse_ticket_url("https://acme.atlassian.net/browse/ENG-123"),
            jira("acme.atlassian.net", "ENG-123")
        );
        assert_eq!(
            parse_ticket_url(" https://Acme.Atlassian.net/browse/eng-123?focusedCommentId=9 "),
            jira("acme.atlassian.net", "ENG-123")
        );
        assert_eq!(
            parse_ticket_url("https://linear.app/acme/issue/ENG-123/fix-the-crash"),
            linear("acme", "ENG-123")
        );
        assert_eq!(
            parse_ticket_url("https://linear.app/acme/issue/ENG-123/"),
            linear("acme", "ENG-123")
        );
        for bad in [
            "https://acme.atlassian.net/browse/ENG",
            "https://acme.atlassian.net/browse/ENG-0",
            "https://acme.atlassian.net/jira/ENG-1",
            "https://linear.app/acme/project/ENG-1",
            "https://linear.app//issue/ENG-1",
            "ftp://acme.atlassian.net/browse/ENG-1",
            "https://github.com/o/r/issues/1",
        ] {
            assert_eq!(parse_ticket_url(bad), None, "{bad}");
        }
        assert_eq!(split_key("ENG_2-12"), Some(("ENG_2", 12)));
        assert_eq!(split_key("eng-12"), None);
    }

    #[tokio::test]
    async fn routed_sends_tickets_to_their_tracker_and_the_rest_to_github() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"key": "ENG-7", "fields": {"summary": "From Jira", "labels": []}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let auth = JiraAuth {
            email: "bot@acme.test".into(),
            token: "tok-x".into(),
        };
        let routed = Routed {
            gh: Gh {
                program: "/nonexistent/gh".into(),
            },
            remotes: HashMap::from([(
                "acme/api".to_string(),
                Remote::Jira(Jira::new(&server.uri(), "acme.atlassian.net", "ENG", auth)),
            )]),
        };
        // Slugs are case-insensitive, as everywhere else in Provefab.
        assert_eq!(
            routed.issue("Acme/API", 7).await.unwrap().title,
            "From Jira"
        );
        // A stored pending label effect (slug, number) replays to the same tracker.
        routed
            .edit_labels("acme/api", 7, &["provefab:in-pr"], &["provefab"])
            .await
            .unwrap();
        // Another repository's issues and every forge call go to gh.
        assert!(matches!(
            routed.issue("o/r", 7).await,
            Err(ForgeError::Spawn { .. })
        ));
        assert!(matches!(
            routed
                .pr_status("acme/api", "https://github.com/acme/api/pull/1")
                .await,
            Err(ForgeError::Spawn { .. })
        ));
    }

    const CONFIG: &str = "[jev]\nmodel = \"jev-1.13\"\n\n[[models]]\nid = \"m\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n\n[[repos]]\nslug = \"acme/api\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n\n[[repos]]\nslug = \"acme/web\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"linear\"\nproject = \"WEB\"\n\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n";

    #[tokio::test]
    async fn routed_is_built_from_the_config_and_names_missing_credentials() {
        let config = Config::from_toml_str(CONFIG).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let nothing = fake_security(dir.path(), "security", "exit 44");
        let env = env_of(&[
            ("PROVEFAB_JIRA_EMAIL", "e@acme.test"),
            ("PROVEFAB_JIRA_TOKEN", "tok"),
            ("PROVEFAB_LINEAR_KEY", "lin"),
        ]);
        let routed = Routed::from_config(
            &config,
            Gh {
                program: "gh".into(),
            },
            &nothing,
            &env,
        )
        .await
        .unwrap();
        assert!(
            matches!(routed.remote("ACME/api"), Some(Remote::Jira(j)) if j.api == "https://acme.atlassian.net" && j.project == "ENG")
        );
        assert!(
            matches!(routed.remote("acme/web"), Some(Remote::Linear(l)) if l.api == crate::linear::API && l.team == "WEB")
        );
        assert!(routed.remote("o/r").is_none());
        let err = Routed::from_config(
            &config,
            Gh {
                program: "gh".into(),
            },
            &nothing,
            &env_of(&[]),
        )
        .await
        .err()
        .unwrap();
        assert!(
            err.contains("provefab login jira --site acme.atlassian.net"),
            "{err}"
        );
    }
}
