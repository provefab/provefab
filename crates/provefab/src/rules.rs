//! Repository rules (docs/specs/2026-10-01-repo-rules-design.md): the
//! conventions a repository keeps in `.provefab/rules.md`, read from the base
//! commit, given to the stages and checked by the reviewer. The format,
//! selection and rendering are pure.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agents::StageRunner;
use crate::config::RepoConfig;
use crate::forge::PrState;
use crate::pipeline::{PR_COMMENT_FILE, Pipeline, PipelineError, pass_of};
use crate::policy::{BoxFuture, PeriodicTools, Signal, SignalKind};
use crate::ports::{Hub, Oracle};
use crate::post_merge::CheckState;
use crate::record::{Disposition, Event};
use crate::risk::{UNMATCHABLE, glob_match, unmatchable};
use crate::stage::ReviewOutput;
use crate::store::{MaintenanceRun, TaskRow, Write, now};
use crate::task::{Stage, TaskState};

/// Where a repository keeps its rules, from its root (spec §1).
pub const PATH: &str = ".provefab/rules.md";
/// Spec §3 limits.
pub const MAX_RULES: usize = 100;
pub const MAX_SUMMARY: usize = 120;
pub const MAX_TEXT: usize = 2000;
/// Characters of rules one prompt carries (spec §5).
pub const BUDGET: usize = 12_000;
/// The block's title in every prompt (spec §5).
pub const TITLE: &str = "Repository rules (approved by the maintainers of this repository)";
/// What the review prompt adds after the block when it carries rules (spec §5).
pub const REVIEW_ASK: &str = "When the change breaks one of these rules, report it as a finding with `rule` set to that rule's number (for example \"R3\"). Every other finding has `rule` null.\n";

/// One rule of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub number: u32,
    pub summary: String,
    /// `paths:` patterns; empty: the rule applies whatever the files.
    pub paths: Vec<String>,
    /// The rule's Markdown text, without its trailing blank lines.
    pub text: String,
    /// The bytes of the file this rule covers, its heading up to the next
    /// rule heading or the end: where Provefab Pro edits a rule, leaving every
    /// other byte as the maintainers wrote it (plan decision 7).
    pub span: Range<usize>,
}

/// Why a rules file is invalid (spec §3). Never quotes the file: the reason
/// goes to the task log, `provefab doctor` and the export as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    #[error(
        "line {0}: a rule heading is `## R<number>: <summary>`, the number from 1, without leading zeros"
    )]
    Heading(usize),
    #[error("R{0} is used by two rules")]
    Duplicate(u32),
    #[error("line {line}: `{key}` goes once, between a rule heading and its text")]
    Misplaced { line: usize, key: &'static str },
    #[error("R{rule}: path pattern {index} {why}")]
    Pattern {
        rule: u32,
        index: usize,
        why: &'static str,
    },
    #[error("more than 100 rules")]
    TooMany,
    #[error("R{0}: the summary is longer than 120 characters")]
    SummaryTooLong(u32),
    #[error("R{0}: the text is longer than 2000 characters")]
    TextTooLong(u32),
}

/// Parses a rules file (spec §3, plan decision 6). Text before the first
/// rule heading is an introduction and is ignored, including any `## `
/// heading there that does not start with `R` and a digit; after it, every
/// `## ` line is a rule heading. A rule's optional `paths:` and `sources:`
/// lines come right after its heading, then its text; `sources:` is never
/// interpreted.
pub fn parse(text: &str) -> Result<Vec<Rule>, RulesError> {
    let mut rules: Vec<Rule> = Vec::new();
    let mut open: Option<Draft> = None;
    let mut at = 0;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let start = at;
        at += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let line = if i == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        let rule_like =
            line.starts_with("## R") && line[4..].starts_with(|c: char| c.is_ascii_digit());
        if line.starts_with("## ") && (open.is_some() || rule_like) {
            let (number, summary) = heading(line).ok_or(RulesError::Heading(i + 1))?;
            if let Some(d) = open.take() {
                push(&mut rules, d.finish(start)?)?;
            }
            open = Some(Draft::new(number, summary, start));
        } else if let Some(d) = open.as_mut() {
            d.line(i + 1, line)?;
        }
    }
    if let Some(d) = open.take() {
        push(&mut rules, d.finish(text.len())?)?;
    }
    Ok(rules)
}

fn push(rules: &mut Vec<Rule>, rule: Rule) -> Result<(), RulesError> {
    if rules.iter().any(|r| r.number == rule.number) {
        return Err(RulesError::Duplicate(rule.number));
    }
    if rules.len() == MAX_RULES {
        return Err(RulesError::TooMany);
    }
    rules.push(rule);
    Ok(())
}

/// `## R<n>: <summary>` as (n, summary).
fn heading(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix("## R")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (number, tail) = rest.split_at(digits);
    if number.is_empty() || number.starts_with('0') {
        return None;
    }
    let summary = tail.strip_prefix(':')?.trim();
    Some((number.parse().ok()?, summary)).filter(|(_, s)| !s.is_empty())
}

/// A rule being read, line by line.
struct Draft<'a> {
    number: u32,
    summary: &'a str,
    paths: Option<Vec<String>>,
    sources: bool,
    text: Vec<&'a str>,
    start: usize,
}

impl<'a> Draft<'a> {
    fn new(number: u32, summary: &'a str, start: usize) -> Self {
        Self {
            number,
            summary,
            paths: None,
            sources: false,
            text: Vec::new(),
            start,
        }
    }

    fn line(&mut self, n: usize, line: &'a str) -> Result<(), RulesError> {
        for key in ["paths:", "sources:"] {
            if let Some(value) = line.strip_prefix(key) {
                let seen = if key == "paths:" {
                    self.paths.is_some()
                } else {
                    self.sources
                };
                if seen || !self.text.is_empty() {
                    return Err(RulesError::Misplaced { line: n, key });
                }
                if key == "paths:" {
                    self.paths = Some(value.split(',').map(|p| p.trim().to_string()).collect());
                } else {
                    self.sources = true;
                }
                return Ok(());
            }
        }
        if !self.text.is_empty() || !line.trim().is_empty() {
            self.text.push(line);
        }
        Ok(())
    }

    fn finish(self, end: usize) -> Result<Rule, RulesError> {
        let number = self.number;
        if self.summary.chars().count() > MAX_SUMMARY {
            return Err(RulesError::SummaryTooLong(number));
        }
        let paths = self.paths.unwrap_or_default();
        for (i, p) in paths.iter().enumerate() {
            let why = if p.is_empty() {
                Some("is empty")
            } else if unmatchable(p) {
                Some(UNMATCHABLE)
            } else {
                None
            };
            if let Some(why) = why {
                return Err(RulesError::Pattern {
                    rule: number,
                    index: i + 1,
                    why,
                });
            }
        }
        let text = self.text.join("\n").trim_end().to_string();
        if text.chars().count() > MAX_TEXT {
            return Err(RulesError::TextTooLong(number));
        }
        Ok(Rule {
            number,
            summary: self.summary.to_string(),
            paths,
            text,
            span: self.start..end,
        })
    }
}

/// The rules a stage gets (spec §5): every rule for the plan; for the
/// implementation (the plan's files) and the review (the round's changed
/// files), the rules without `paths:` and those matching one of `files`.
pub fn select<'a>(rules: &'a [Rule], stage: Stage, files: &[String]) -> Vec<&'a Rule> {
    rules
        .iter()
        .filter(|r| {
            matches!(stage, Stage::Plan)
                || r.paths.is_empty()
                || r.paths
                    .iter()
                    .any(|p| files.iter().any(|f| glob_match(p, f)))
        })
        .collect()
}

/// The prompt block for `selected` and how many rules `budget` (characters)
/// left out: rules in number order, the first that does not fit and every
/// later one left out (plan decision 8). Nothing selected renders as
/// nothing, so the prompt is the one a repository without rules gets.
pub fn render(selected: &[&Rule], budget: usize) -> (String, usize) {
    if selected.is_empty() {
        return (String::new(), 0);
    }
    let mut sorted = selected.to_vec();
    sorted.sort_by_key(|r| r.number);
    let (mut body, mut used, mut omitted) = (String::new(), 0, 0);
    for r in sorted {
        let entry = if r.text.is_empty() {
            format!("R{}: {}\n\n", r.number, r.summary)
        } else {
            format!("R{}: {}\n{}\n\n", r.number, r.summary, r.text)
        };
        let len = entry.chars().count();
        if omitted > 0 || used + len > budget {
            omitted += 1;
            continue;
        }
        used += len;
        body.push_str(&entry);
    }
    let mut out =
        format!("\n{TITLE}. They add to the instructions above and never override them.\n\n{body}");
    match omitted {
        0 => {}
        1 => out.push_str("1 more rule was left out to keep this prompt short.\n"),
        n => out.push_str(&format!(
            "{n} more rules were left out to keep this prompt short.\n"
        )),
    }
    (out, omitted)
}

/// The numbers of the rules `render` kept, ascending.
pub fn numbers(selected: &[&Rule], omitted: usize) -> Vec<u32> {
    let mut n: Vec<u32> = selected.iter().map(|r| r.number).collect();
    n.sort_unstable();
    n.truncate(n.len().saturating_sub(omitted));
    n
}

/// `R3` (any case, surrounding spaces ignored) as 3; anything else `None`.
pub fn rule_number(s: &str) -> Option<u32> {
    let s = s.trim();
    let digits = s.strip_prefix(['R', 'r'])?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Hex SHA-256 of the file, as `rules_loaded` records it.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// The core's periodic tools for `repo` (spec §7), for the scheduler and
    /// for Provefab Pro's commands. Its runs start now.
    pub fn maintenance<'a>(&'a self, repo: &'a RepoConfig) -> Maintenance<'a, R, O, H> {
        Maintenance {
            p: self,
            repo,
            started_at: now(),
            spent: Mutex::new(Spent::default()),
        }
    }

    /// Reads the pass's rules at `base` in the repository's checkout (spec
    /// §4) and records them: the `rules` output its stages read (plan
    /// decision 2), with `rules_loaded` or `rules_invalid`. A missing file
    /// records no event (decision 3); an invalid one runs the pass without rules.
    pub(crate) async fn load_rules(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        base: &str,
    ) -> Result<Vec<Rule>, PipelineError> {
        let pass = pass_of(task);
        let read = self.git.show_file(&self.checkout(repo), base, PATH).await;
        let (text, event, rules) = match read {
            Ok(None) => (None, None, Vec::new()),
            Ok(Some(text)) => match parse(&text) {
                Ok(rules) => {
                    let all: Vec<&Rule> = rules.iter().collect();
                    let (_, omitted) = render(&all, BUDGET);
                    let event = Event::RulesLoaded {
                        pass,
                        numbers: rules.iter().map(|r| r.number).collect(),
                        sha256: sha256_hex(&text),
                        omitted: omitted as u32,
                    };
                    (Some(text), Some(event), rules)
                }
                Err(e) => {
                    let reason = e.to_string();
                    (None, Some(Event::RulesInvalid { pass, reason }), Vec::new())
                }
            },
            Err(e) => {
                // A git error can name paths or hold a token: logged, never recorded.
                eprintln!(
                    "provefab: task {}: could not read {PATH} at the base commit: {e}",
                    task.id
                );
                let reason = format!("could not read {PATH} at the base commit");
                (None, Some(Event::RulesInvalid { pass, reason }), Vec::new())
            }
        };
        let value = json!({"pass": pass, "base": base, "text": text});
        let events: Vec<Event> = event.into_iter().collect();
        self.store
            .write_with_events(
                task.id,
                Write::Output {
                    kind: "rules",
                    value: &value,
                },
                &events,
            )
            .await?;
        Ok(rules)
    }

    /// The rules of the task's current pass: those `prepare` loaded at the
    /// pass's pinned base, or loaded now from it when the pass has none
    /// (begun before the upgrade, Review Focus 5) or has rules read at another
    /// commit (a `prepare` run again in the pass pins the base again).
    pub(crate) async fn pass_rules(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<Vec<Rule>, PipelineError> {
        let base = self.pass_base(task, repo).await?;
        if let Some(v) = self.store.last_output(task.id, "rules").await?
            && v["pass"].as_u64() == Some(u64::from(pass_of(task)))
            && v["base"].as_str() == Some(base.as_str())
        {
            return Ok(v["text"]
                .as_str()
                .and_then(|t| parse(t).ok())
                .unwrap_or_default());
        }
        self.load_rules(task, repo, &base).await
    }

    /// `R1, R3`: the rules the current round's review was given (plan
    /// decision 10), `None` when it was given none.
    pub(crate) async fn rules_given(
        &self,
        task: &TaskRow,
    ) -> Result<Option<String>, PipelineError> {
        let (pass, round) = (u64::from(pass_of(task)), u64::from(task.review_rounds));
        let given = self
            .store
            .recent_outputs(task.id, "rules_given", u32::MAX)
            .await?
            .into_iter()
            .rev()
            .find(|v| v["pass"].as_u64() == Some(pass) && v["round"].as_u64() == Some(round));
        Ok(given.and_then(|v| {
            let names: Vec<String> = v["numbers"]
                .as_array()?
                .iter()
                .filter_map(Value::as_u64)
                .map(|n| format!("R{n}"))
                .collect();
            (!names.is_empty()).then(|| names.join(", "))
        }))
    }
}

/// The core's `PeriodicTools` for one repository (spec §7). The cost of
/// its model calls waits here until a run is recorded (plan decision 15).
pub struct Maintenance<'a, R, O, H> {
    p: &'a Pipeline<R, O, H>,
    repo: &'a RepoConfig,
    started_at: i64,
    spent: Mutex<Spent>,
}

/// Model calls not yet written with a run.
#[derive(Debug, Default)]
struct Spent {
    model_id: Option<String>,
    cost_usd: Option<f64>,
    quota_units: Option<f64>,
}

/// `41` for `https://github.com/o/r/pull/41`.
fn pr_number(url: &str) -> Option<u64> {
    url.rsplit_once("/pull/")?
        .1
        .split(['/', '#', '?'])
        .next()?
        .parse()
        .ok()
}

/// `pr#<n>/<what>` for a pull request URL, else `task#<id>/<what>`.
fn signal_id(task: &TaskRow, url: &str, what: &str) -> String {
    match pr_number(url) {
        Some(n) => format!("pr#{n}/{what}"),
        None => format!("task#{}/{what}", task.id),
    }
}

/// A tool's error is fixed text (pre-flight S1): the detail, which can hold
/// a path or a token, goes to the log only, redacted.
fn failed(said: String, detail: impl std::fmt::Display) -> String {
    eprintln!(
        "provefab: {said}: {}",
        redact_credentials(&detail.to_string())
    );
    said
}

/// `text` with every word that looks like a credential replaced by
/// `<redacted>` (pre-flight S6). Signals carry what people and reviewers
/// wrote to the model provider; `tracker::redact` needs the secrets it
/// removes, and these are anyone's, so this goes by shape.
fn redact_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        if word.is_empty() {
            out.push_str(piece);
            continue;
        }
        if after_bearer || looks_like_credential(word) {
            out.push_str("<redacted>");
        } else {
            out.push_str(word);
        }
        out.push_str(&piece[word.len()..]);
        after_bearer = word.eq_ignore_ascii_case("bearer");
    }
    out
}

/// Token prefixes of GitHub, GitLab, Slack, AWS and the model providers.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "sk-",
    "akia",
];
/// What the name of an assigned credential holds (`GH_TOKEN=...`).
const CREDENTIAL_NAMES: &[&str] = &["token", "secret", "password", "passwd", "key", "auth"];

fn looks_like_credential(word: &str) -> bool {
    let w = word.trim_matches(|c: char| "\"'`()[]{}<>,;.".contains(c));
    let lower = w.to_ascii_lowercase();
    let prefixed = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .any(|part| {
            TOKEN_PREFIXES
                .iter()
                .any(|p| part.starts_with(p) && part.len() >= p.len() + 12)
        });
    let assigned = lower.split_once(['=', ':']).is_some_and(|(name, value)| {
        !value.is_empty() && CREDENTIAL_NAMES.iter().any(|n| name.contains(n))
    });
    let userinfo = lower
        .split_once("://")
        .is_some_and(|(_, rest)| rest.split('/').next().is_some_and(|h| h.contains('@')));
    // Long, opaque and mixed case: a key, not a word or a (lowercase) sha.
    let opaque = w.len() >= 32
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-+/=".contains(c))
        && w.chars().any(|c| c.is_ascii_uppercase())
        && w.chars().any(|c| c.is_ascii_lowercase())
        && w.chars().any(|c| c.is_ascii_digit());
    prefixed || assigned || userinfo || opaque
}

impl<R, O, H> Maintenance<'_, R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Plan decision 17.
    async fn collect(&self, since: i64) -> Result<Vec<Signal>, PipelineError> {
        let (p, slug) = (self.p, &self.repo.slug);
        let policy = crate::risk::resolve(self.repo.risk.as_ref()).unwrap_or_default();
        let configured: HashSet<&str> = self
            .repo
            .gates
            .iter()
            .chain(policy.categories.iter().flat_map(|c| &c.checks))
            .map(String::as_str)
            .collect();
        let mut out = Vec::new();
        for task in p.store.tasks_in(&TaskState::ALL).await? {
            if !task.repo.eq_ignore_ascii_case(slug) {
                continue;
            }
            let events = p.store.events(task.id).await?;
            let typed: Vec<(i64, Event)> = events
                .iter()
                .filter_map(|e| Some((e.at, e.typed()?)))
                .collect();
            let fallback = task
                .pr_url
                .clone()
                .unwrap_or_else(|| task.issue_url.clone());
            let pr_of_pass = |pass: u32| {
                typed.iter().find_map(|(_, e)| match e {
                    Event::PrOpened { url, pass: n, .. } if *n == pass => Some(url.clone()),
                    _ => None,
                })
            };
            let pr_before = |at: i64| {
                typed.iter().rev().find_map(|(t, e)| match e {
                    Event::PrOpened { url, .. } if *t <= at => Some(url.clone()),
                    _ => None,
                })
            };
            for f in p.store.findings(task.id).await? {
                let reviewed = events
                    .iter()
                    .find(|e| e.id == f.event_id)
                    .map_or(0, |e| e.at);
                let decided = typed.iter().rev().find_map(|(at, e)| match e {
                    Event::FindingDisposition {
                        finding,
                        disposition,
                        reason,
                        ..
                    } if *finding == f.key => Some((*at, *disposition, reason.clone())),
                    _ => None,
                });
                let rule = f.rule.as_deref().and_then(rule_number);
                let refused = decided.as_ref().is_some_and(|(_, d, _)| {
                    matches!(d, Disposition::Rejected | Disposition::Waived)
                });
                let at = decided
                    .as_ref()
                    .map_or(reviewed, |(a, _, _)| (*a).max(reviewed));
                if !(refused || rule.is_some()) || at < since {
                    continue;
                }
                let url = pr_of_pass(f.pass).unwrap_or_else(|| fallback.clone());
                let (disposition, reason) = match decided {
                    Some((_, d, r)) => (Some(d), r),
                    None => (None, None),
                };
                out.push(Signal {
                    id: signal_id(&task, &url, &f.key),
                    at,
                    url,
                    kind: SignalKind::Finding {
                        key: f.key.clone(),
                        file: redact_credentials(&f.file),
                        text: redact_credentials(&f.text),
                        rule,
                        disposition,
                        reason: reason.as_deref().map(redact_credentials),
                    },
                });
            }
            for (at, v) in p.store.outputs_since(task.id, "review", since).await? {
                let Ok(review) = serde_json::from_value::<ReviewOutput>(v) else {
                    continue;
                };
                let url = pr_before(at).unwrap_or_else(|| fallback.clone());
                let asked = review
                    .findings
                    .into_iter()
                    .filter(|f| f.file == PR_COMMENT_FILE);
                for (i, f) in asked.enumerate() {
                    out.push(Signal {
                        id: signal_id(&task, &url, &format!("c{}", i + 1)),
                        at,
                        url: url.clone(),
                        kind: SignalKind::ChangeRequest {
                            text: redact_credentials(&f.text),
                        },
                    });
                }
            }
            // Configured commands only: the plan's reproduction command, the
            // last gate, is model-written and never leaves the machine.
            let (mut failing, mut last): (Vec<String>, i64) = (Vec::new(), 0);
            for (at, e) in typed.iter().filter(|(at, _)| *at >= since) {
                match e {
                    Event::GatesRun { results, .. } => {
                        for r in results.iter().filter(|r| !r.passed) {
                            let command = redact_credentials(&r.command);
                            if configured.contains(r.command.as_str())
                                && !failing.contains(&command)
                            {
                                failing.push(command);
                                last = last.max(*at);
                            }
                        }
                    }
                    Event::PostMerge {
                        check_id, state, ..
                    } if state == CheckState::RevertOpen.as_str() => {
                        out.push(Signal {
                            id: format!("task#{}/revert-{check_id}", task.id),
                            at: *at,
                            url: fallback.clone(),
                            kind: SignalKind::Revert,
                        });
                    }
                    Event::IssueReopened { previous_pass } => out.push(Signal {
                        id: format!("task#{}/reopen-{previous_pass}", task.id),
                        at: *at,
                        url: task.issue_url.clone(),
                        kind: SignalKind::Reopen,
                    }),
                    _ => {}
                }
            }
            if !failing.is_empty() {
                out.push(Signal {
                    id: format!("task#{}/gates", task.id),
                    at: last,
                    url: fallback.clone(),
                    kind: SignalKind::GateFailure { commands: failing },
                });
            }
        }
        // Every earlier periodic pull request that is merged or closed, whatever its age.
        let mut seen = HashSet::new();
        for run in p
            .store
            .maintenance_runs(Some(slug))
            .await?
            .into_iter()
            .rev()
        {
            let Some(url) = run.pr_url.clone() else {
                continue;
            };
            if !seen.insert(url.clone()) {
                continue;
            }
            let merged = match p.hub.pr_status(slug, &url).await {
                Ok(s) => match s.state {
                    PrState::Merged => true,
                    PrState::Closed => false,
                    PrState::Open => continue,
                },
                Err(e) => {
                    failed(format!("could not read {url}"), e);
                    continue;
                }
            };
            let what = if merged { "merged" } else { "closed" };
            out.push(Signal {
                id: format!("pr#{}/{what}", pr_number(&url).unwrap_or_default()),
                at: run.finished_at.unwrap_or(run.started_at),
                url,
                kind: SignalKind::Proposal {
                    kind: run.kind,
                    merged,
                    detail: run.detail,
                },
            });
        }
        out.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        Ok(out)
    }

    async fn highest(&self) -> Result<u32, PipelineError> {
        let mut high = 0;
        for task in self.p.store.tasks_in(&TaskState::ALL).await? {
            if !task.repo.eq_ignore_ascii_case(&self.repo.slug) {
                continue;
            }
            for e in self.p.store.events(task.id).await? {
                if e.kind == "rules_loaded"
                    && let Some(Event::RulesLoaded { numbers, .. }) = e.typed()
                {
                    high = numbers.into_iter().fold(high, u32::max);
                }
            }
            for f in self.p.store.findings(task.id).await? {
                if let Some(n) = f.rule.as_deref().and_then(rule_number) {
                    high = high.max(n);
                }
            }
        }
        Ok(high)
    }

    async fn base_rules(&self) -> Result<Option<String>, String> {
        let (p, repo) = (self.p, self.repo);
        let lock = p.repo_lock(repo);
        let _guard = lock.lock().await;
        let not_fetched = || format!("could not fetch {}", repo.slug);
        match p.refresh_checkout(repo).await {
            Ok(true) => {}
            // `refresh_checkout` logged why.
            Ok(false) => return Err(not_fetched()),
            Err(e) => return Err(failed(not_fetched(), e)),
        }
        let base = p.base_ref(repo).await;
        p.git
            .show_file(&p.checkout(repo), &base, PATH)
            .await
            .map_err(|e| {
                let said = format!("could not read {PATH} on the base branch of {}", repo.slug);
                failed(said, e)
            })
    }

    async fn record(
        &self,
        kind: &str,
        outcome: &str,
        pr_url: Option<&str>,
        detail: Option<&Value>,
    ) -> Result<(), String> {
        let spent = std::mem::take(
            &mut *self
                .spent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let run = MaintenanceRun {
            id: 0,
            repo: self.repo.slug.clone(),
            kind: kind.into(),
            started_at: self.started_at,
            finished_at: Some(now()),
            model_id: spent.model_id,
            cost_usd: spent.cost_usd,
            quota_units: spent.quota_units,
            outcome: outcome.into(),
            pr_url: pr_url.map(Into::into),
            detail: detail.cloned(),
        };
        self.p
            .store
            .record_maintenance_run(&run)
            .await
            .map(|_| ())
            .map_err(|e| {
                failed(
                    format!("could not record the {kind} run of {}", run.repo),
                    e,
                )
            })
    }
}

impl<R, O, H> PeriodicTools for Maintenance<'_, R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    fn signals(&self, since: i64) -> BoxFuture<'_, Result<Vec<Signal>, String>> {
        Box::pin(async move {
            self.collect(since).await.map_err(|e| {
                failed(
                    format!("could not read the record of {}", self.repo.slug),
                    e,
                )
            })
        })
    }

    fn highest_rule_number(&self) -> BoxFuture<'_, Result<u32, String>> {
        Box::pin(async move {
            self.highest().await.map_err(|e| {
                failed(
                    format!("could not read the record of {}", self.repo.slug),
                    e,
                )
            })
        })
    }

    fn rules_at_base(&self) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(self.base_rules())
    }

    fn last_run<'a>(
        &'a self,
        kind: &'a str,
    ) -> BoxFuture<'a, Result<Option<MaintenanceRun>, String>> {
        Box::pin(async move {
            self.p
                .store
                .last_maintenance_run(&self.repo.slug, kind)
                .await
                .map_err(|e| {
                    failed(
                        format!("could not read the {kind} runs of {}", self.repo.slug),
                        e,
                    )
                })
        })
    }

    fn record_run<'a>(
        &'a self,
        kind: &'a str,
        outcome: &'a str,
        pr_url: Option<&'a str>,
        detail: Option<&'a Value>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.record(kind, outcome, pr_url, detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_looking_words_are_redacted_and_prose_is_not() {
        for (said, want) in [
            ("use GH_TOKEN=ghp_abcdefghijklmnop0123", "use <redacted>"),
            (
                "key `sk-ant-api03-abcdefghijklmn` leaked",
                "key <redacted> leaked",
            ),
            (
                "Authorization: Bearer abc.def",
                "Authorization: Bearer <redacted>",
            ),
            (
                "clone https://me:pw@github.com/o/r\nnow",
                "clone <redacted>\nnow",
            ),
            ("aws AKIAIOSFODNN7EXAMPLE1 here", "aws <redacted> here"),
            (
                "x Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRg y",
                "x <redacted> y",
            ),
        ] {
            assert_eq!(redact_credentials(said), want, "{said}");
        }
        for prose in [
            "use the existing helper, not anyhow",
            "the task-runner and sk-learn stay",
            "commit 0123456789abcdef0123456789abcdef01234567 broke it",
            "the key: is the domain's word\n\n  indented",
            "see https://github.com/o/r/pull/41",
        ] {
            assert_eq!(redact_credentials(prose), prose);
        }
    }

    const FILE: &str = "# Our rules\n\nA human introduction.\n\n## About\n\nIgnored too.\n\n## R3: Errors in the API layer use ApiError, never anyhow\npaths: src/api/**, src/web/*.rs\nsources: PR #41 F2 (rejected), PR #57 (closed with a change request)\n\nReturn `ApiError` from handlers; `anyhow` stays in the CLI.\n\n## R7: Keep pull requests small\n\nOne change per pull request.\n### Why\nReviews stay short.\n";

    #[test]
    fn a_valid_file_parses_with_its_introduction_ignored() {
        let rules = parse(FILE).unwrap();
        assert_eq!(rules.iter().map(|r| r.number).collect::<Vec<_>>(), [3, 7]);
        let r3 = &rules[0];
        assert_eq!(
            r3.summary,
            "Errors in the API layer use ApiError, never anyhow"
        );
        assert_eq!(r3.paths, ["src/api/**", "src/web/*.rs"]);
        assert_eq!(
            r3.text,
            "Return `ApiError` from handlers; `anyhow` stays in the CLI."
        );
        let section = &FILE[r3.span.clone()];
        assert!(
            section.starts_with("## R3: ") && section.ends_with("CLI.\n\n"),
            "{section:?}"
        );
        assert_eq!(
            rules[1].text,
            "One change per pull request.\n### Why\nReviews stay short."
        );
        assert!(rules[1].paths.is_empty());
        assert_eq!(rules[1].span.end, FILE.len());
    }

    #[test]
    fn a_missing_or_empty_file_means_no_rules() {
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse("# Rules\n\nNone yet.\n"), Ok(vec![]));
    }

    #[test]
    fn every_validation_error_is_named() {
        for (text, want) in [
            ("## R1 Missing colon\n", RulesError::Heading(1)),
            ("## R0: Zero\n", RulesError::Heading(1)),
            ("## R01: Leading zero\n", RulesError::Heading(1)),
            ("## R1:   \n", RulesError::Heading(1)),
            ("## R1: One\n\ntext\n## Notes\n", RulesError::Heading(4)),
            ("## R1: One\n\n## R1: Again\n", RulesError::Duplicate(1)),
            (
                "## R1: One\npaths: a/**\npaths: b/**\n",
                RulesError::Misplaced {
                    line: 3,
                    key: "paths:",
                },
            ),
            (
                "## R1: One\n\ntext\nsources: PR #1\n",
                RulesError::Misplaced {
                    line: 4,
                    key: "sources:",
                },
            ),
            (
                "## R1: One\npaths: a/**, \n",
                RulesError::Pattern {
                    rule: 1,
                    index: 2,
                    why: "is empty",
                },
            ),
            (
                "## R1: One\npaths: /src/**\n",
                RulesError::Pattern {
                    rule: 1,
                    index: 1,
                    why: UNMATCHABLE,
                },
            ),
            (
                "## R1: One\npaths: src/\n",
                RulesError::Pattern {
                    rule: 1,
                    index: 1,
                    why: UNMATCHABLE,
                },
            ),
        ] {
            assert_eq!(parse(text), Err(want), "{text:?}");
        }
    }

    #[test]
    fn limits_are_inclusive() {
        let many = |n: u32| {
            (1..=n)
                .map(|i| format!("## R{i}: Rule {i}\n\ntext\n\n"))
                .collect::<String>()
        };
        assert_eq!(parse(&many(100)).unwrap().len(), 100);
        assert_eq!(parse(&many(101)), Err(RulesError::TooMany));
        let summary = |n: usize| format!("## R1: {}\n", "s".repeat(n));
        assert!(parse(&summary(120)).is_ok());
        assert_eq!(parse(&summary(121)), Err(RulesError::SummaryTooLong(1)));
        // Characters, not bytes.
        let text = |n: usize| format!("## R1: One\n\n{}\n", "é".repeat(n));
        assert!(parse(&text(2000)).is_ok());
        assert_eq!(parse(&text(2001)), Err(RulesError::TextTooLong(1)));
    }

    /// Plan decision 5: the reason goes to the log, doctor and the export as is.
    #[test]
    fn errors_never_quote_the_file() {
        for text in [
            "## R1: SENTINEL_42\npaths: /SENTINEL_42/**\n",
            "## RSENTINEL_42\n## R1: x\n\n## R1: SENTINEL_42\n",
            "## R1: x\n\nSENTINEL_42\n## SENTINEL_42\n",
        ] {
            let e = parse(text).unwrap_err().to_string();
            assert!(!e.contains("SENTINEL_42"), "{e}");
        }
    }

    /// Review Focus 1.
    #[test]
    fn windows_line_endings_and_a_byte_order_mark_read_like_plain_text() {
        let plain = "## R1: One\npaths: src/**\n\nText.\n\n## R2: Two\n\nMore.\n";
        let windows = format!("\u{feff}{}", plain.replace('\n', "\r\n"));
        let strip = |rs: Vec<Rule>| {
            rs.into_iter()
                .map(|r| (r.number, r.summary, r.paths, r.text))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip(parse(&windows).unwrap()),
            strip(parse(plain).unwrap())
        );
    }

    fn rule(number: u32, paths: &[&str], text: &str) -> Rule {
        Rule {
            number,
            summary: format!("Rule {number}"),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            text: text.into(),
            span: 0..0,
        }
    }

    #[test]
    fn the_plan_gets_every_rule_the_other_stages_those_that_match() {
        let rules = [
            rule(1, &[], "a"),
            rule(2, &["src/api/**"], "b"),
            rule(3, &["docs/*.md"], "c"),
        ];
        let numbers = |s: Vec<&Rule>| s.iter().map(|r| r.number).collect::<Vec<_>>();
        let api = ["src/api/handler.rs".to_string()];
        assert_eq!(numbers(select(&rules, Stage::Plan, &[])), [1, 2, 3]);
        assert_eq!(numbers(select(&rules, Stage::Implement, &api)), [1, 2]);
        assert_eq!(
            numbers(select(&rules, Stage::Review, &["README.md".to_string()])),
            [1]
        );
    }

    #[test]
    fn rendering_keeps_number_order_and_counts_what_the_budget_leaves_out() {
        // Each entry is 1913 characters: six fit in 12 000, four do not.
        let rules: Vec<Rule> = (1..=10)
            .rev()
            .map(|n| rule(n, &[], &"x".repeat(1900)))
            .collect();
        let all: Vec<&Rule> = rules.iter().collect();
        let (text, omitted) = render(&all, BUDGET);
        assert_eq!(omitted, 4);
        assert!(text.starts_with(&format!("\n{TITLE}.")), "{text}");
        assert!(text.find("R1: Rule 1\n").unwrap() < text.find("R6: Rule 6\n").unwrap());
        assert!(!text.contains("R7: Rule 7"), "left out in number order");
        assert!(
            text.ends_with("4 more rules were left out to keep this prompt short.\n"),
            "{text}"
        );
        assert_eq!(numbers(&all, omitted), [1, 2, 3, 4, 5, 6]);
        let two = [rule(1, &[], "a"), rule(2, &[], &"x".repeat(1990))];
        let (text, omitted) = render(&two.iter().collect::<Vec<_>>(), 100);
        assert_eq!(omitted, 1);
        assert!(text.ends_with("1 more rule was left out to keep this prompt short.\n"));
    }

    #[test]
    fn no_rule_renders_nothing_and_a_rule_without_text_is_its_summary() {
        assert_eq!(render(&[], BUDGET), (String::new(), 0));
        let r = rule(4, &[], "");
        assert_eq!(
            render(&[&r], BUDGET).0,
            format!(
                "\n{TITLE}. They add to the instructions above and never override them.\n\nR4: Rule 4\n\n"
            )
        );
    }

    #[test]
    fn rule_numbers_are_r_and_a_number() {
        assert_eq!(rule_number(" R3 "), Some(3));
        assert_eq!(rule_number("r12"), Some(12));
        for bad in ["3", "R", "R03", "R0", "Rx", "R3a", ""] {
            assert_eq!(rule_number(bad), None, "{bad}");
        }
    }

    /// A base the checkout cannot read stores Provefab's fixed reason and no
    /// text, never git's own words (which can name paths or hold a token).
    #[cfg(feature = "testkit")]
    #[tokio::test]
    async fn an_unreadable_base_records_only_the_fixed_reason() {
        use crate::testkit::{FakeHub, FakeOracle, fixture, happy, pipeline, queue};
        let f = fixture(&["true"]);
        let p = pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await;
        let id = queue(&p).await;
        let task = p.store.task(id).await.unwrap().unwrap();
        let zero = "0".repeat(40);
        let rules = p
            .load_rules(&task, &f.config.repos[0], &zero)
            .await
            .unwrap();
        assert!(rules.is_empty());
        let out = p.store.last_output(id, "rules").await.unwrap().unwrap();
        assert_eq!(out, json!({"pass": 1, "base": zero, "text": null}));
        let ev = p.store.events(id).await.unwrap();
        let invalid: Vec<_> = ev.iter().filter(|e| e.kind == "rules_invalid").collect();
        assert_eq!(invalid.len(), 1);
        assert_eq!(
            invalid[0].payload["reason"],
            "could not read .provefab/rules.md at the base commit"
        );
    }

    /// The PR's `Rules:` line reads the current pass and round only: a later
    /// round that selected no rules records nothing and shows no line, and a
    /// round recorded twice reads its latest record.
    #[cfg(feature = "testkit")]
    #[tokio::test]
    async fn rules_given_reads_the_current_pass_and_round_only() {
        use crate::testkit::{FakeHub, FakeOracle, fixture, happy, pipeline, queue};
        let f = fixture(&["true"]);
        let p = pipeline(
            &f,
            Box::new(happy),
            FakeOracle::default(),
            FakeHub::new("x"),
        )
        .await;
        let id = queue(&p).await;
        let task = p.store.task(id).await.unwrap().unwrap();
        assert_eq!(p.rules_given(&task).await.unwrap(), None);
        let (pass, round) = (pass_of(&task), task.review_rounds);
        for numbers in [json!([1]), json!([1, 3])] {
            p.store
                .record_output(
                    id,
                    "rules_given",
                    &json!({"pass": pass, "round": round, "numbers": numbers}),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            p.rules_given(&task).await.unwrap().as_deref(),
            Some("R1, R3")
        );
        p.store.bump_review_rounds(id).await.unwrap();
        let later = p.store.task(id).await.unwrap().unwrap();
        assert_eq!(p.rules_given(&later).await.unwrap(), None);
    }
}
