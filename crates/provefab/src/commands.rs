//! The user-facing commands besides `run` and `guard` (spec §2.2): `add`,
//! `status`, `log`, `doctor`, plus the single-instance lock and the Jev key.

use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::config::Config;
use crate::forge::{ForgeError, Git};
use crate::paths::Paths;
use crate::ports::Hub;
use crate::post_merge::CheckState;
use crate::record::{Event, MergedBy, StoredEvent, parse_date, redact_event, redact_text};
use crate::store::{NewIssue, Store, StoreError};
use crate::task::TaskState;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("`{0}` is not a GitHub issue URL (https://github.com/owner/name/issues/N)")]
    BadUrl(String),
    #[error("{0} is not in provefab.toml's [[repos]]")]
    UnknownRepo(String),
    #[error("no task {0}")]
    UnknownTask(i64),
    #[error("{0}")]
    BadDate(&'static str),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Forge(#[from] ForgeError),
}

/// `https://github.com/owner/name/issues/N` → (`owner/name`, N).
pub fn parse_issue_url(url: &str) -> Option<(String, u64)> {
    let rest = url.trim().trim_end_matches('/');
    let rest = rest
        .strip_prefix("https://github.com/")
        .or_else(|| rest.strip_prefix("http://github.com/"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [owner, name, "issues", n] if !owner.is_empty() && !name.is_empty() => {
            Some((format!("{owner}/{name}"), n.parse().ok()?))
        }
        _ => None,
    }
}

/// `provefab add <url>`: queues the issue, or requeues it when the task is
/// parked (terminal or waiting for information). A task in progress is left alone.
pub async fn add<H: Hub>(
    store: &Store,
    config: &Config,
    hub: &H,
    git: &Git,
    paths: &Paths,
    url: &str,
) -> Result<String, CommandError> {
    let (slug, number) = parse_issue_url(url).ok_or_else(|| CommandError::BadUrl(url.into()))?;
    let repo = config
        .repos
        .iter()
        .find(|r| r.slug.eq_ignore_ascii_case(&slug))
        .ok_or(CommandError::UnknownRepo(slug))?;
    let issue = hub.issue(&repo.slug, number).await?;
    let new = NewIssue {
        repo: repo.slug.clone(),
        number,
        url: issue.url.clone(),
        title: issue.title.clone(),
        author: issue.author.clone(),
    };
    if let Some(id) = store.add_issue(&new).await? {
        return Ok(format!("queued as task {id}"));
    }
    let task = store
        .tasks_in(&TaskState::ALL)
        .await?
        .into_iter()
        .find(|t| t.repo.eq_ignore_ascii_case(&repo.slug) && t.issue_number == number)
        .ok_or(CommandError::UnknownTask(0))?;
    if task.state.is_terminal() || task.state == TaskState::NeedsInfo {
        // A new pass starts from `base` on a new branch (the pipeline suffixes it
        // with the pass number): the old worktree holds work that was rejected
        // or already shipped. Review findings stay in the store (D46).
        crate::pipeline::start_pass(
            store,
            hub,
            git,
            paths,
            repo,
            &task,
            "requeued by provefab add",
            crate::pipeline::PassOptions::default(),
        )
        .await?;
        Ok(format!(
            "task {} requeued (was {})",
            task.id,
            task.state.as_str()
        ))
    } else {
        Ok(format!(
            "task {} is already in progress ({})",
            task.id,
            task.state.as_str()
        ))
    }
}

/// `provefab status`: one line per task, with the reason for its current state.
pub async fn status(store: &Store) -> Result<String, CommandError> {
    let mut out = String::new();
    let tasks = store.tasks_in(&TaskState::ALL).await?;
    if tasks.is_empty() {
        out.push_str("no tasks\n");
    }
    for t in tasks {
        let why = store
            .transitions(t.id)
            .await?
            .last()
            .map(|r| r.reason.clone())
            .unwrap_or_default();
        let why = why.lines().next().unwrap_or_default();
        let _ = writeln!(
            out,
            "{:>4}  {}#{}  {:<12} {}{}",
            t.id,
            t.repo,
            t.issue_number,
            t.state.as_str(),
            why,
            t.pr_url.map(|u| format!("  {u}")).unwrap_or_default(),
        );
    }
    let counts = store.post_merge_state_counts().await?;
    if !counts.is_empty() {
        let parts: Vec<String> = counts
            .iter()
            .map(|(s, n)| format!("{} {n}", s.as_str()))
            .collect();
        let _ = writeln!(out, "post-merge checks: {}", parts.join(" · "));
    }
    Ok(out)
}

/// `provefab stats`: per repository, how many issues became pull requests,
/// how they ended, and which reviewers approved the merged ones. What a pilot
/// team measures (landing L14).
pub async fn stats(store: &Store) -> Result<String, CommandError> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Repo {
        tasks: u32,
        prs: u32,
        merged: u32,
        auto: u32,
        closed: u32,
        reopened_after_merge: u32,
        post_merge_passed: u32,
        post_merge_flaky: u32,
        post_merge_superseded: u32,
        revert_prs: u32,
        post_merge_blocked: u32,
        stopped: u32,
        seconds_to_pr: Vec<i64>,
        reviewers: BTreeMap<String, u32>,
    }
    let mut repos: BTreeMap<String, Repo> = BTreeMap::new();
    for t in store.tasks_in(&TaskState::ALL).await? {
        let r = repos.entry(t.repo.clone()).or_default();
        r.tasks += 1;
        if matches!(t.state, TaskState::Failed | TaskState::NeedsYou) {
            r.stopped += 1;
        }
        let transitions = store.transitions(t.id).await?;
        if t.pr_url.is_none() {
            continue;
        }
        r.prs += 1;
        if let (Some(first), Some(opened)) = (
            transitions.first(),
            transitions.iter().find(|x| x.to == TaskState::PrOpen),
        ) {
            r.seconds_to_pr.push(opened.at - first.at);
        }
        for check in store.post_merge_checks(t.id).await? {
            match check.state {
                CheckState::Passed => {
                    r.post_merge_passed += 1;
                    if !check.flaky.is_empty() {
                        r.post_merge_flaky += 1;
                    }
                }
                CheckState::Superseded => r.post_merge_superseded += 1,
                CheckState::RevertOpen => r.revert_prs += 1,
                CheckState::Blocked => r.post_merge_blocked += 1,
                _ => {}
            }
        }
        match t.pr_state.as_deref() {
            Some("merged" | "done") => {
                r.merged += 1;
                if store.last_output(t.id, "auto_merged").await?.is_some() {
                    r.auto += 1;
                }
                let models: Vec<String> = store
                    .recent_outputs(t.id, "approval", u32::MAX)
                    .await?
                    .iter()
                    .filter_map(|a| a["model"].as_str().map(str::to_string))
                    .collect();
                if !models.is_empty() {
                    *r.reviewers.entry(models.join(" + ")).or_default() += 1;
                }
                if t.reopen_count > 0 && t.state != TaskState::PrOpen {
                    r.reopened_after_merge += 1;
                }
            }
            Some("closed") => r.closed += 1,
            _ => {}
        }
    }
    if repos.is_empty() {
        return Ok("no tasks\n".into());
    }
    let mut out = String::new();
    for (slug, r) in repos {
        let mut secs = r.seconds_to_pr.clone();
        secs.sort_unstable();
        let median = secs.get(secs.len() / 2).map_or("-".to_string(), |&t| {
            if t < 60 {
                format!("{t} s")
            } else {
                format!("{} min", t / 60)
            }
        });
        let pct = (r.prs * 100).checked_div(r.tasks).unwrap_or(0);
        let _ = writeln!(
            out,
            "{slug}: {} tasks · {} PRs ({pct}%) · merged {} (auto {}, by hand {}) · closed {} · stopped {} · reopened after merge {} · post-merge passed {} · flaky {} · superseded {} · reverts opened {} · blocked {}",
            r.tasks,
            r.prs,
            r.merged,
            r.auto,
            r.merged - r.auto,
            r.closed,
            r.stopped,
            r.reopened_after_merge,
            r.post_merge_passed,
            r.post_merge_flaky,
            r.post_merge_superseded,
            r.revert_prs,
            r.post_merge_blocked
        );
        let _ = writeln!(out, "  median issue to PR: {median}");
        let pairs: Vec<String> = r
            .reviewers
            .iter()
            .map(|(k, n)| format!("{k} ({n})"))
            .collect();
        let _ = writeln!(
            out,
            "  reviewers: {}",
            if pairs.is_empty() {
                "-".to_string()
            } else {
                pairs.join(", ")
            }
        );
    }
    Ok(out)
}

/// `provefab log <task>`: transitions, routing, stage runs and stage answers.
pub async fn log(store: &Store, id: i64) -> Result<String, CommandError> {
    let t = store.task(id).await?.ok_or(CommandError::UnknownTask(id))?;
    let mut out = format!(
        "task {} {}#{} \"{}\" ({})\nstate {}  kind {}  attempts {}  review rounds {}{}\n",
        t.id,
        t.repo,
        t.issue_number,
        t.title,
        t.issue_url,
        t.state.as_str(),
        t.kind.map(|k| k.as_str()).unwrap_or("-"),
        t.attempts,
        t.review_rounds,
        t.pr_url.map(|u| format!("  pr {u}")).unwrap_or_default()
    );
    out.push_str("\ntransitions:\n");
    for r in store.transitions(id).await? {
        let from = r.from.map(|s| s.as_str()).unwrap_or("-");
        let _ = writeln!(out, "  {} {from} -> {}: {}", r.at, r.to.as_str(), r.reason);
    }
    out.push_str("\nrouting:\n");
    for (jev, verdict, tiers, reasons) in store.routing_decisions(id).await? {
        let _ = writeln!(
            out,
            "  jev {}  verdict {}  tiers {tiers}",
            jev.unwrap_or_else(|| "unavailable".into()),
            verdict.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
        );
        for r in reasons {
            let _ = writeln!(out, "    {r}");
        }
    }
    let routes = store.recent_outputs(id, "route", 50).await?;
    if !routes.is_empty() {
        out.push_str("\nroutes:\n");
        for r in routes {
            let _ = writeln!(
                out,
                "  {} -> {}: {}",
                r["stage"].as_str().unwrap_or("-"),
                r["model"].as_str().unwrap_or("-"),
                r["why"].as_str().unwrap_or("-")
            );
        }
    }
    out.push_str("\nstage runs:\n");
    let runs = store.stage_runs(id).await?;
    for r in &runs {
        let cache = if r.cache_read_tokens + r.cache_write_tokens > 0 {
            format!(" (+cache {}/{})", r.cache_read_tokens, r.cache_write_tokens)
        } else {
            String::new()
        };
        let cost = match (r.cost_usd, r.quota_units) {
            (Some(d), _) => format!("  cost {}", crate::cost::usd(d)),
            (None, Some(q)) => format!("  quota {q:.2}"),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "  {} {:<10} {:<12} {}  turns {}  tokens {}/{}{cache}{cost}{}  {}",
            r.started_at,
            r.stage,
            if r.model_id.is_empty() {
                "-"
            } else {
                &r.model_id
            },
            r.exit,
            r.turns,
            r.input_tokens,
            r.output_tokens,
            r.gate_score
                .as_ref()
                .map(|s| format!("  score {s}"))
                .unwrap_or_default(),
            r.session_dir.display()
        );
    }
    if let Some(total) = crate::cost::summary(&runs) {
        let _ = writeln!(out, "  total: {total}");
    }
    let post_merge = store.post_merge_checks(id).await?;
    if !post_merge.is_empty() {
        out.push_str("\npost-merge checks:\n");
        for c in post_merge {
            let _ = writeln!(
                out,
                "  #{} {} {} on {}{}",
                c.id,
                c.state.as_str(),
                c.merge_sha,
                c.base,
                c.failure_kind
                    .map(|k| format!("  ({})", k.as_str()))
                    .unwrap_or_default()
            );
            for (label, v) in [
                ("base tip", &c.base_sha),
                ("revert commit", &c.revert_sha),
                ("revert PR", &c.revert_pr_url),
            ] {
                if let Some(v) = v {
                    let _ = writeln!(out, "    {label}: {v}");
                }
            }
            if !c.failed_commands.is_empty() {
                let failed = crate::post_merge::failure_summary(&c.failed_commands)
                    .lines()
                    .map(|l| format!("      {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = writeln!(out, "    failed:\n{failed}");
            }
            if !c.flaky.is_empty() {
                let _ = writeln!(out, "    flaky: {}", c.flaky.join(", "));
            }
            if let Some(s) = &c.failure_summary {
                let _ = writeln!(out, "    detail: {s}");
            }
        }
    }
    let events = store.events(id).await?;
    if !events.is_empty() {
        out.push_str("\nrecord:\n");
        for e in &events {
            let _ = writeln!(out, "  {} [{}] {}", e.seq, e.source, event_summary(e));
        }
    }
    let findings = store.findings(id).await?;
    if !findings.is_empty() {
        let disp = store.current_dispositions(id).await?;
        out.push_str("findings:\n");
        for f in findings {
            let place = match (&f.file, f.line) {
                (file, Some(l)) => format!("{file}:{l}"),
                (file, None) if !file.is_empty() => file.clone(),
                _ => "-".to_string(),
            };
            let _ = writeln!(
                out,
                "  {} · {} · {place} · round {} · {}",
                f.key,
                f.severity,
                f.round,
                disp.get(&f.key).map(|d| d.as_str()).unwrap_or("open")
            );
        }
    }
    for kind in ["plan", "review", "failure"] {
        if let Some(v) = store.last_output(id, kind).await? {
            let _ = writeln!(
                out,
                "\nlast {kind}:\n{}",
                serde_json::to_string_pretty(&v).unwrap_or_default()
            );
        }
    }
    Ok(out)
}

/// The kind and its identifying fields; free text never appears here.
fn event_summary(e: &StoredEvent) -> String {
    use Event::*;
    let Some(ev) = e.typed() else {
        return format!("{} (unknown to this version)", e.kind);
    };
    match ev {
        StageRun {
            stage, model_id, ..
        } => format!("{} {stage} {model_id}", e.kind),
        GatesRun { stage, round, .. } => format!("{} {stage} round {round}", e.kind),
        Reproduction {
            failed_before_fix, ..
        } => format!("{} failed_before_fix {failed_before_fix}", e.kind),
        PrOpened { url, pass, .. } => format!("{} {url} pass {pass}", e.kind),
        Merged { by, pass, .. } => {
            let by = match by {
                MergedBy::Auto => "auto",
                MergedBy::Human => "human",
            };
            format!("{} by {by} pass {pass}", e.kind)
        }
        PostMerge {
            check_id, state, ..
        } => format!("{} #{check_id} {state}", e.kind),
        IssueReopened { previous_pass } => format!("{} after pass {previous_pass}", e.kind),
        Plan { pass, .. } => format!("{} pass {pass}", e.kind),
        Review {
            reviewer_model,
            pass,
            round,
            verdict,
            ..
        } => format!(
            "{} {reviewer_model} pass {pass} round {round} {verdict}",
            e.kind
        ),
        FindingDisposition {
            finding,
            disposition,
            login,
            ..
        } => format!("{} {finding} {} ({login})", e.kind, disposition.as_str()),
        CommandIgnored { login, why, .. } => format!("{} ({login}) {why}", e.kind),
        FindingInferred {
            finding,
            rule_version,
            ..
        } => format!("{} {finding} (rule v{rule_version})", e.kind),
        Routed { .. } => e.kind.clone(),
    }
}

/// The record as JSON Lines: one object per event, then per finding. Free
/// text is redacted (length and digest) unless `with_text`. Command output is
/// never included: events carry only a reference to it.
pub async fn export(
    store: &Store,
    repo: Option<&str>,
    since: Option<&str>,
    with_text: bool,
) -> Result<String, CommandError> {
    let since = since
        .map(|s| parse_date(s).ok_or(CommandError::BadDate("--since must be YYYY-MM-DD")))
        .transpose()?;
    let (events, findings) = store.export_rows(repo, since).await?;
    let mut out = String::new();
    let mut line = |v: serde_json::Value| {
        out.push_str(&serde_json::to_string(&v).unwrap_or_default());
        out.push('\n');
    };
    for (t, e) in events {
        line(serde_json::json!({
            "type": "event", "task": t.id, "repo": t.repo, "issue": t.issue_number,
            "seq": e.seq, "kind": e.kind, "source": e.source,
            "schema_version": e.schema_version, "at": e.at,
            "payload": if with_text { e.payload.clone() } else { redact_event(&e) },
        }));
    }
    for (t, f, d) in findings {
        line(serde_json::json!({
            "type": "finding", "task": t.id, "key": f.key, "pass": f.pass,
            "round": f.round, "reviewer_model": f.reviewer_model,
            "severity": f.severity, "file": f.file, "line": f.line,
            "text": if with_text { serde_json::Value::String(f.text.clone()) } else { redact_text(&f.text) },
            "disposition": d.map(|d| d.as_str()),
        }));
    }
    Ok(out)
}

/// Deletes the record of finished tasks last updated before `before`
/// (`YYYY-MM-DD`); a dry run unless `yes`.
pub async fn prune(store: &Store, before: &str, yes: bool) -> Result<String, CommandError> {
    let at = parse_date(before).ok_or(CommandError::BadDate("--before must be YYYY-MM-DD"))?;
    let tasks = store.prunable_tasks(at).await?;
    let mut out = String::new();
    for t in &tasks {
        let _ = writeln!(out, "task {} {}#{}", t.id, t.repo, t.issue_number);
    }
    if yes {
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        let (events, findings) = store.prune_record(&ids).await?;
        let _ = writeln!(out, "deleted {events} events, {findings} findings");
    } else {
        out.push_str("dry run: pass --yes to delete\n");
    }
    Ok(out)
}

/// One process drives the queue at a time: a second `provefab run` would race
/// the first on the same tasks. The lock lasts as long as the returned file.
pub fn lock(path: &Path) -> std::io::Result<Option<File>> {
    use std::os::fd::AsRawFd;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(path)?;
    // SAFETY: flock on a descriptor we own; no memory is shared.
    let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        Ok(Some(file))
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

/// Keychain service name for the TypeSafe key (`security add-generic-password
/// -s provefab-typesafe -a provefab -w <key>`).
pub const KEYCHAIN_SERVICE: &str = "provefab-typesafe";

/// Keychain service holding the user's Anthropic API key (bring your own key).
pub const ANTHROPIC_KEYCHAIN_SERVICE: &str = "provefab-anthropic";

/// Writes `apiKeyHelper` into the API-key config dir's `settings.json`, keeping
/// any other setting: Claude Code then asks the Keychain for the key at each
/// refresh, and the key never enters the agent's environment.
pub fn claude_api_settings(config_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("settings.json");
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v["apiKeyHelper"] = serde_json::Value::String(format!(
        "security find-generic-password -s {ANTHROPIC_KEYCHAIN_SERVICE} -a provefab -w"
    ));
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// `TYPESAFE_API_KEY`, else the macOS Keychain (spec §2.3).
pub async fn typesafe_key() -> Option<String> {
    if let Some(k) = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    {
        return Some(k.trim().to_string());
    }
    let out = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !key.is_empty()).then_some(key)
}

/// One doctor finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// The programs doctor inspects; tests point them at fakes.
#[derive(Debug, Clone)]
pub struct Tools {
    pub git: PathBuf,
    pub gh: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub pi: PathBuf,
    /// macOS `security`, to check the Keychain holds an API key.
    pub security: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
            claude: "claude".into(),
            codex: "codex".into(),
            pi: "pi".into(),
            security: "security".into(),
        }
    }
}

async fn probe(program: &Path, args: &[&str], envs: &[(&str, &Path)]) -> Result<String, String> {
    probe_text(program, args, envs)
        .await
        .map(|t| t.lines().next().unwrap_or_default().trim().to_string())
}

/// Runs the program; its whole output on success, its first line on failure.
async fn probe_text(
    program: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "no answer after 20s".to_string())?
        .map_err(|e| format!("not runnable: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let first = text.lines().next().unwrap_or_default().trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(if first.is_empty() {
            format!("exit {:?}", out.status.code())
        } else {
            first
        })
    }
}

/// `claude auth status` prints JSON; only `"loggedIn": true` counts.
fn claude_login(text: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|_| format!("unexpected output: {}", text.trim()))?;
    if v["loggedIn"] == serde_json::Value::Bool(true) {
        Ok(format!(
            "logged in with {} ({})",
            v["authMethod"].as_str().unwrap_or("?"),
            v["subscriptionType"].as_str().unwrap_or("?")
        ))
    } else {
        Err("not logged in: run `provefab login claude`".into())
    }
}

fn check(name: &str, r: Result<String, String>) -> Check {
    match r {
        Ok(detail) => Check {
            name: name.into(),
            ok: true,
            detail,
        },
        Err(detail) => Check {
            name: name.into(),
            ok: false,
            detail,
        },
    }
}

/// Asks Jev one tiny question with the configured model: a missing key, a bad
/// key and an unknown model all fail here instead of silently in every task.
pub async fn jev_check(client: &jev::JevClient) -> Check {
    let mut q = jev::Questions::new();
    q.insert(
        "ping".into(),
        jev::Question::noul("Is this text a greeting?"),
    );
    let asked = tokio::time::timeout(
        crate::jevq::DEADLINE * 3,
        client.evaluate(&serde_json::json!("hello"), &q),
    )
    .await;
    match asked {
        Ok(Ok(r)) => Check {
            name: "jev".into(),
            ok: true,
            detail: format!("{} answered", r.model),
        },
        Ok(Err(e)) => Check {
            name: "jev".into(),
            ok: false,
            detail: format!(
                "{e} (check jev.model in provefab.toml: a full version such as `jev-1.13.0`)"
            ),
        },
        Err(_) => Check {
            name: "jev".into(),
            ok: false,
            detail: "no answer in time".into(),
        },
    }
}

/// `provefab doctor` (spec §2.2, §7): tools, logins, Jev, repos.
/// Prices doctor reports (D71): the table in use, and each model's price.
/// Doctor never fetches: the cache, else the built-in snapshot.
fn price_checks(config: &Config, paths: &Paths) -> Vec<Check> {
    use crate::prices;
    let table = prices::cached(paths).unwrap_or_else(prices::snapshot);
    let age = if table.source == "snapshot" {
        "built-in snapshot".to_string()
    } else {
        format!(
            "{} h old",
            (crate::store::now() - table.fetched_at).max(0) / 3600
        )
    };
    let mut checks = vec![Check {
        name: "prices".into(),
        ok: true,
        detail: format!("{}, {age}, {} models", table.source, table.models.len()),
    }];
    for m in &config.models {
        let detail = match prices::price_of(m, &table) {
            Some(p) => {
                let key = if m.price_in.is_some() && m.price_out.is_some() {
                    "set in provefab.toml".to_string()
                } else {
                    m.price_id
                        .clone()
                        .or_else(|| table.key_for(m))
                        .unwrap_or_default()
                };
                format!("${}/${} per M ({key})", p.input, p.output)
            }
            // A warning, not a failure: the model stays usable, ranked last.
            None => "no price: ranked last; set price_id or price_in/price_out".into(),
        };
        checks.push(Check {
            name: format!("price {}", m.id),
            ok: true,
            detail,
        });
    }
    checks
}

pub async fn doctor(
    tools: &Tools,
    config: &Config,
    paths: &Paths,
    jev: Option<&jev::JevClient>,
) -> Vec<Check> {
    let mut checks = vec![
        check("git", probe(&tools.git, &["--version"], &[]).await),
        check("gh", probe(&tools.gh, &["--version"], &[]).await),
        check("gh login", probe(&tools.gh, &["auth", "status"], &[]).await),
    ];
    use crate::config::{Auth, WorkerKind};
    let uses = |w: WorkerKind| config.models.iter().any(|m| m.worker == w);
    let uses_mode =
        |w: WorkerKind, a: Auth| config.models.iter().any(|m| m.worker == w && m.auth == a);
    if uses(WorkerKind::ClaudeCode) {
        checks.push(check(
            "claude",
            probe(&tools.claude, &["--version"], &[]).await,
        ));
    }
    if uses_mode(WorkerKind::ClaudeCode, Auth::Subscription) {
        let claude_config = paths.claude_config();
        checks.push(check(
            "claude login",
            probe_text(
                &tools.claude,
                &["auth", "status"],
                &[("CLAUDE_CONFIG_DIR", claude_config.as_path())],
            )
            .await
            .and_then(|t| claude_login(&t)),
        ));
    }
    if uses_mode(WorkerKind::ClaudeCode, Auth::ApiKey) {
        let fix = "run `provefab login claude --api-key`";
        let key = probe(
            &tools.security,
            &[
                "find-generic-password",
                "-s",
                ANTHROPIC_KEYCHAIN_SERVICE,
                "-a",
                "provefab",
            ],
            &[],
        )
        .await
        .map(|_| format!("Keychain item `{ANTHROPIC_KEYCHAIN_SERVICE}`"))
        .map_err(|_| format!("no Keychain item `{ANTHROPIC_KEYCHAIN_SERVICE}`: {fix}"));
        let helper = std::fs::read_to_string(paths.claude_config_api().join("settings.json"))
            .is_ok_and(|t| t.contains(ANTHROPIC_KEYCHAIN_SERVICE));
        checks.push(check(
            "claude api key",
            key.and_then(|k| {
                if helper {
                    Ok(k)
                } else {
                    Err(format!(
                        "the API-key config dir does not read the Keychain: {fix}"
                    ))
                }
            }),
        ));
    }
    if uses(WorkerKind::Codex) {
        checks.push(check(
            "codex",
            probe(&tools.codex, &["--version"], &[]).await,
        ));
    }
    if uses_mode(WorkerKind::Codex, Auth::Subscription) {
        let codex_home = paths.codex_home();
        checks.push(check(
            "codex login",
            probe(
                &tools.codex,
                &["login", "status"],
                &[("CODEX_HOME", codex_home.as_path())],
            )
            .await,
        ));
        checks.push(check(
            "codex guard hook",
            crate::codex_setup::check(&tools.codex, &codex_home, &codex_home)
                .await
                .map(|_| "trusted, and no project is".to_string())
                .map_err(|e| format!("{e} (run `provefab login codex`)")),
        ));
    }
    if uses_mode(WorkerKind::Codex, Auth::ApiKey) {
        let home = paths.codex_home_api();
        let fix = "run `provefab login codex --api-key`";
        checks.push(check(
            "codex api login",
            probe_text(
                &tools.codex,
                &["login", "status"],
                &[("CODEX_HOME", home.as_path())],
            )
            .await
            .and_then(|t| {
                if t.to_lowercase().contains("api key") {
                    Ok(t.lines().next().unwrap_or_default().to_string())
                } else {
                    Err(format!("not signed in with an API key: {fix}"))
                }
            })
            .map_err(|e| {
                if e.contains(fix) {
                    e
                } else {
                    format!("{e}: {fix}")
                }
            }),
        ));
        checks.push(check(
            "codex api guard hook",
            crate::codex_setup::check(&tools.codex, &home, &home)
                .await
                .map(|_| "trusted, and no project is".to_string())
                .map_err(|e| format!("{e} ({fix})")),
        ));
    }
    if uses(crate::config::WorkerKind::Pi) {
        checks.push(check("pi", probe(&tools.pi, &["--version"], &[]).await));
    }
    match jev {
        Some(client) => checks.push(jev_check(client).await),
        None => checks.push(Check {
            name: "jev key".into(),
            ok: false,
            detail: format!(
                "missing: set TYPESAFE_API_KEY or add Keychain item `{KEYCHAIN_SERVICE}`; routing falls back to the standard tier"
            ),
        }),
    }
    checks.extend(price_checks(config, paths));
    for repo in &config.repos {
        let path = repo.path();
        if repo.managed() && !path.join(".git").exists() {
            checks.push(Check {
                name: format!("repo {}", repo.slug),
                ok: true,
                detail: format!("managed; cloned into {} on first task", path.display()),
            });
        } else {
            checks.push(check(
                &format!("repo {}", repo.slug),
                probe(
                    &tools.git,
                    &[
                        "-C",
                        &path.display().to_string(),
                        "rev-parse",
                        "--verify",
                        &repo.base,
                    ],
                    &[],
                )
                .await
                .map(|_| format!("{} on {}", path.display(), repo.base))
                .map_err(|e| format!("{}: {e}", path.display())),
            ));
        }
        checks.push(Check {
            name: format!("post-merge {}", repo.slug),
            ok: !repo
                .post_merge_checks
                .iter()
                .any(|g| gate_shares_build_dir(g)),
            detail: if repo.post_merge_checks.is_empty() {
                "off (opt in with post_merge_checks)".to_string()
            } else if repo
                .post_merge_checks
                .iter()
                .any(|g| gate_shares_build_dir(g))
            {
                "a shared build directory may test another commit; use worktree-local build output"
                    .to_string()
            } else {
                format!(
                    "{} command(s); reverts always require human review",
                    repo.post_merge_checks.len()
                )
            },
        });
        let shares_build_dir = repo.gates.iter().any(|g| gate_shares_build_dir(g));
        checks.push(Check {
            name: format!("gates {}", repo.slug),
            ok: !shares_build_dir,
            detail: if shares_build_dir {
                "a build directory shared by all tasks lets a gate test another task's code; drop it or use RUSTC_WRAPPER=sccache".to_string()
            } else {
                "no shared build directory".to_string()
            },
        });
    }
    checks
}

/// True if `gate` sets `CARGO_TARGET_DIR` or `--target-dir` to a fixed
/// (absolute) path, which every task worktree would then share and build
/// into, letting a gate silently test another task's compiled code.
fn gate_shares_build_dir(gate: &str) -> bool {
    let tokens: Vec<&str> = gate.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        let path = if let Some(value) = token.strip_prefix("CARGO_TARGET_DIR=") {
            Some(value)
        } else if let Some(value) = token.strip_prefix("--target-dir=") {
            Some(value)
        } else if *token == "--target-dir" {
            tokens.get(i + 1).copied()
        } else {
            None
        };
        if path.is_some_and(|p| Path::new(p).is_absolute()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BYOK: Claude Code reads the key from the Keychain through `apiKeyHelper`,
    /// so the key never sits in the agent's environment; other settings stay.
    #[test]
    fn claude_api_settings_point_at_the_keychain_and_keep_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        claude_api_settings(dir.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(
            v["apiKeyHelper"],
            "security find-generic-password -s provefab-anthropic -a provefab -w"
        );
        // A fresh dir gets the file too.
        let fresh = tempfile::tempdir().unwrap();
        claude_api_settings(&fresh.path().join("claude-api")).unwrap();
        assert!(fresh.path().join("claude-api/settings.json").exists());
    }

    #[test]
    fn issue_urls() {
        assert_eq!(
            parse_issue_url("https://github.com/o/r/issues/12"),
            Some(("o/r".into(), 12))
        );
        assert_eq!(
            parse_issue_url(" https://github.com/o/r/issues/12/ "),
            Some(("o/r".into(), 12))
        );
        for bad in [
            "https://github.com/o/r/pull/12",
            "https://github.com/o/issues/12",
            "https://gitlab.com/o/r/issues/1",
            "https://github.com/o/r/issues/x",
            "https://github.com/o/r/issues/1/comments",
        ] {
            assert_eq!(parse_issue_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn gate_command_shares_build_dir() {
        assert!(gate_shares_build_dir("CARGO_TARGET_DIR=/x cargo test"));
        assert!(gate_shares_build_dir("cargo test --target-dir /x"));
        assert!(!gate_shares_build_dir("cargo test"));
        assert!(!gate_shares_build_dir("RUSTC_WRAPPER=sccache cargo test"));
        assert!(!gate_shares_build_dir(
            "CARGO_TARGET_DIR=target/x cargo test"
        ));
    }

    #[test]
    fn a_second_lock_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.lock");
        let first = lock(&path).unwrap();
        assert!(first.is_some());
        assert!(lock(&path).unwrap().is_none());
        drop(first);
        assert!(lock(&path).unwrap().is_some());
    }

    fn fake(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[tokio::test]
    async fn doctor_reports_each_tool_and_login() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let tools = Tools {
            git: fake(d, "git", "echo 'git version 2.50'"),
            gh: fake(
                d,
                "gh",
                "if [ \"$1\" = auth ]; then echo 'not logged in' >&2; exit 1; fi; echo 'gh version 2.80'",
            ),
            claude: fake(
                d,
                "claude",
                "if [ \"$1\" = auth ]; then echo \"{\\\"loggedIn\\\": false, \\\"dir\\\": \\\"$CLAUDE_CONFIG_DIR\\\"}\"; exit 0; fi; echo '2.1.281 (Claude Code)'",
            ),
            codex: d.join("missing-codex"),
            pi: d.join("missing-pi"),
            security: d.join("missing-security"),
        };
        let config = Config::from_toml_str(
            r#"
[jev]
model = "jev-1.13"
[[models]]
id = "c"
worker = "claude-code"
model = "sonnet"
tier = "standard"
"#,
        )
        .unwrap();
        let checks = doctor(&tools, &config, &Paths::new(d), None).await;
        let get = |n: &str| checks.iter().find(|c| c.name == n).cloned().unwrap();
        assert!(get("git").ok && get("gh").ok);
        assert_eq!(
            (get("gh login").ok, get("gh login").detail.as_str()),
            (false, "not logged in")
        );
        // Exit 0 with loggedIn false is still a failed login.
        assert!(!get("claude login").ok, "{:?}", get("claude login"));
        assert_eq!(
            claude_login(
                r#"{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}"#
            ),
            Ok("logged in with claude.ai (max)".to_string())
        );
        assert!(!get("jev key").ok);
        // Workers the catalog does not use are not checked.
        assert!(checks.iter().all(|c| c.name != "codex" && c.name != "pi"));
    }

    #[tokio::test]
    async fn doctor_reports_prices_and_unpriced_models() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let tools = Tools {
            git: fake(d, "git", "echo 'git version 2.50'"),
            gh: fake(d, "gh", "echo 'gh version 2.80'"),
            claude: fake(d, "claude", "echo '2.1.281 (Claude Code)'"),
            codex: fake(d, "codex", "echo 'codex 0.50'"),
            pi: d.join("missing-pi"),
            security: d.join("missing-security"),
        };
        let paths = Paths::new(d);
        let mut table = crate::prices::PriceTable::from_models_dev(
            include_str!("../tests/fixtures/prices/models_dev.json"),
            crate::store::now() - 7200,
        )
        .unwrap();
        table.source = "models.dev".into();
        std::fs::write(paths.prices(), serde_json::to_string(&table).unwrap()).unwrap();
        let config = Config::from_toml_str(
            r#"
[jev]
model = "jev-1.13"
[[models]]
id = "c"
worker = "claude-code"
model = "sonnet"
tier = "standard"
[[models]]
id = "typo"
worker = "codex"
model = "gpt-typo"
tier = "standard"
auth = "api_key"
"#,
        )
        .unwrap();
        let checks = doctor(&tools, &config, &paths, None).await;
        let get = |n: &str| checks.iter().find(|c| c.name == n).cloned().unwrap();
        let prices = get("prices");
        assert!(
            prices.ok && prices.detail.contains("models.dev"),
            "{prices:?}"
        );
        assert!(prices.detail.contains("2 h"), "{prices:?}");
        let c = get("price c");
        assert!(c.ok && c.detail.contains('$'), "{c:?}");
        let typo = get("price typo");
        assert!(typo.ok && typo.detail.contains("no price"), "{typo:?}");
    }

    /// BYOK: only the sign-in modes the catalog uses are checked, and an
    /// API-key model is checked for its key, not for a subscription login.
    #[tokio::test]
    async fn doctor_checks_the_sign_in_mode_each_model_uses() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let tools = Tools {
            git: fake(d, "git", "echo 'git version 2.50'"),
            gh: fake(d, "gh", "echo 'gh version 2.80'"),
            claude: fake(d, "claude", "echo '2.1.281 (Claude Code)'"),
            codex: fake(
                d,
                "codex",
                "if [ \"$1\" = login ]; then echo \"Logged in using an API key - sk-***\"; exit 0; fi; echo 'codex-cli 0.156.1'",
            ),
            pi: d.join("missing-pi"),
            security: fake(d, "security", "exit 0"),
        };
        let config = Config::from_toml_str(
            r#"
[jev]
model = "jev-1.13.0"
[[models]]
id = "c"
worker = "claude-code"
model = "sonnet"
tier = "standard"
auth = "api_key"
[[models]]
id = "x"
worker = "codex"
model = "gpt-6"
tier = "standard"
auth = "api_key"
"#,
        )
        .unwrap();
        claude_api_settings(&Paths::new(d).claude_config_api()).unwrap();
        let checks = doctor(&tools, &config, &Paths::new(d), None).await;
        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        let get = |n: &str| checks.iter().find(|c| c.name == n).cloned().unwrap();
        assert!(get("claude api key").ok, "{:?}", get("claude api key"));
        assert!(get("codex api login").ok, "{:?}", get("codex api login"));
        assert!(names.contains(&"codex api guard hook"), "{names:?}");
        // No subscription is in use, so no subscription login is checked.
        assert!(
            !names.contains(&"claude login") && !names.contains(&"codex login"),
            "{names:?}"
        );
        // A missing key fails with the command that fixes it.
        let tools = Tools {
            security: fake(d, "security2", "exit 44"),
            ..tools
        };
        let checks = doctor(&tools, &config, &Paths::new(d), None).await;
        let key = checks.iter().find(|c| c.name == "claude api key").unwrap();
        assert!(
            !key.ok && key.detail.contains("provefab login claude --api-key"),
            "{key:?}"
        );
    }

    /// Seen live 2026-09-25: `jev.model = "jev-1.13"` passed the old key-only
    /// check while every Jev call failed with "Unknown model".
    #[tokio::test]
    async fn doctor_asks_jev_a_question_with_the_configured_model() {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({"model": "jev-1.13"})))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-1.13"}}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"model": "jev-1.13.0"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "jev-1.13.0",
                "answers": {"ping": {"type": "noul", "noul": 0.9}},
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .mount(&server)
            .await;
        let client = |model: &str| {
            jev::JevClient::new("k", model, Duration::from_secs(2))
                .unwrap()
                .with_base_url(server.uri())
        };
        let bad = jev_check(&client("jev-1.13")).await;
        assert!(!bad.ok && bad.detail.contains("Unknown model"), "{bad:?}");
        assert!(bad.detail.contains("jev.model"), "{bad:?}");
        let good = jev_check(&client("jev-1.13.0")).await;
        assert!(good.ok && good.detail.contains("jev-1.13.0"), "{good:?}");
    }
}
