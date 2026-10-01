//! The evidence and decision record
//! (docs/specs/2026-09-30-evidence-record-design.md): what Provefab observed,
//! what models claimed, what humans decided and what Provefab inferred.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agents::StageRunner;
use crate::forge::{Comment, is_bot_comment};
use crate::pipeline::{Pipeline, PipelineError};
use crate::ports::{Hub, Oracle};
use crate::store::TaskRow;

/// Inference rules carry their version so a changed rule never mixes with old results.
pub const RULE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Fact,
    Claim,
    Human,
    Inferred,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Claim => "claim",
            Self::Human => "human",
            Self::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Accepted,
    Rejected,
    Fixed,
    Waived,
}

impl Disposition {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "fixed" => Some(Self::Fixed),
            "waived" => Some(Self::Waived),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Fixed => "fixed",
            Self::Waived => "waived",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergedBy {
    Auto,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    UnaddressedAtMerge,
    FollowedByRevert,
    FollowedByReopen,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnaddressedAtMerge => "unaddressed_at_merge",
            Self::FollowedByRevert => "followed_by_revert",
            Self::FollowedByReopen => "followed_by_reopen",
        }
    }
}

/// One gate command's result. `output_ref` is the local session directory,
/// never the output itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateEntry {
    pub command: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub passed: bool,
    pub output_ref: String,
}

/// Every event kind of spec section 3.3 (schema_version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    // fact
    Routed {
        tiers: Value,
        jev_model: Option<String>,
        fallback: bool,
    },
    StageRun {
        stage: String,
        model_id: String,
        actual_model: Option<String>,
        provider: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        cost_usd: Option<f64>,
        exit: String,
    },
    GatesRun {
        stage: String,
        round: u32,
        results: Vec<GateEntry>,
    },
    Reproduction {
        command: String,
        failed_before_fix: bool,
    },
    PrOpened {
        url: String,
        head: Option<String>,
        base: String,
        pass: u32,
    },
    Merged {
        sha: Option<String>,
        base: Option<String>,
        by: MergedBy,
        pass: u32,
    },
    PostMerge {
        check_id: i64,
        state: String,
        failure_kind: Option<String>,
    },
    IssueReopened {
        previous_pass: u32,
    },
    /// The risk categories of one round's change (risk policy §6). Paths are
    /// identity, not free text: exported as is.
    RiskClassified {
        pass: u32,
        round: u32,
        categories: Vec<crate::risk::Detected>,
    },
    // claim
    Plan {
        pass: u32,
        summary: String,
        steps: Vec<String>,
        risks: Vec<String>,
    },
    Review {
        reviewer_model: String,
        pass: u32,
        round: u32,
        verdict: String,
        findings: Vec<String>,
    },
    // human
    FindingDisposition {
        finding: String,
        disposition: Disposition,
        reason: Option<String>,
        login: String,
        association: String,
        comment: String,
    },
    CommandIgnored {
        comment: String,
        login: String,
        line: String,
        why: String,
    },
    // inferred
    FindingInferred {
        finding: String,
        rule: Rule,
        rule_version: u32,
    },
}

impl Event {
    /// The `kind` column. Inferred events are stored under their rule's
    /// spec name (`finding_unaddressed_at_merge`, ...).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Routed { .. } => "routed",
            Self::StageRun { .. } => "stage_run",
            Self::GatesRun { .. } => "gates_run",
            Self::Reproduction { .. } => "reproduction",
            Self::PrOpened { .. } => "pr_opened",
            Self::Merged { .. } => "merged",
            Self::PostMerge { .. } => "post_merge",
            Self::IssueReopened { .. } => "issue_reopened",
            Self::RiskClassified { .. } => "risk_classified",
            Self::Plan { .. } => "plan",
            Self::Review { .. } => "review",
            Self::FindingDisposition { .. } => "finding_disposition",
            Self::CommandIgnored { .. } => "command_ignored",
            Self::FindingInferred { rule, .. } => match rule {
                Rule::UnaddressedAtMerge => "finding_unaddressed_at_merge",
                Rule::FollowedByRevert => "finding_followed_by_revert",
                Rule::FollowedByReopen => "finding_followed_by_reopen",
            },
        }
    }

    pub fn source(&self) -> Source {
        match self {
            Self::Plan { .. } | Self::Review { .. } => Source::Claim,
            Self::FindingDisposition { .. } | Self::CommandIgnored { .. } => Source::Human,
            Self::FindingInferred { .. } => Source::Inferred,
            _ => Source::Fact,
        }
    }

    pub fn schema_version(&self) -> u32 {
        1
    }
}

/// A row of `change_events`. `payload` is kept as JSON so a reader never
/// fails on a kind or version it does not know.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: i64,
    pub task_id: i64,
    pub seq: i64,
    pub kind: String,
    pub source: String,
    pub schema_version: u32,
    pub payload: Value,
    pub at: i64,
}

impl StoredEvent {
    /// `None` for an unknown kind or version: report it, never fail.
    pub fn typed(&self) -> Option<Event> {
        if self.schema_version != 1 {
            return None;
        }
        serde_json::from_value(self.payload.clone()).ok()
    }
}

/// A free-text field as length and digest, so an export can be compared
/// without carrying the text.
pub fn redact_text(s: &str) -> Value {
    let hash = Sha256::digest(s.as_bytes());
    serde_json::json!({
        "redacted": true,
        "len": s.chars().count(),
        "sha256": hash.iter().map(|b| format!("{b:02x}")).collect::<String>(),
    })
}

/// The `stage_run.exit` values that carry no text of their own; any other
/// (`provider_error: ...`, `crashed (...): <stderr>`) is redacted.
const PLAIN_EXITS: &[&str] = &["completed", "max_turns", "timeout", "passed", "failed"];

/// The payload with its free-text fields (per kind) replaced by `redact_text`.
/// An allowlist: a kind or version this build does not know exports
/// `{"unknown": true}`, never its payload.
pub fn redact_event(e: &StoredEvent) -> Value {
    let Some(typed) = e.typed() else {
        return serde_json::json!({"unknown": true});
    };
    let mut p = e.payload.clone();
    let fields: &[&str] = match &typed {
        Event::Plan { .. } => &["summary", "steps", "risks"],
        Event::FindingDisposition { .. } => &["reason"],
        Event::CommandIgnored { .. } => &["line"],
        // Model-written (the plan's reproduction command).
        Event::Reproduction { .. } => &["command"],
        Event::StageRun { exit, .. } if !PLAIN_EXITS.contains(&exit.as_str()) => &["exit"],
        _ => &[],
    };
    for f in fields {
        if let Some(v) = p.get_mut(*f) {
            redact_value(v);
        }
    }
    // The reproduction command also runs as the last gate: redact every gate
    // command, the digest still compares the configured ones across tasks.
    if let Some(Value::Array(results)) = p.get_mut("results")
        && matches!(typed, Event::GatesRun { .. })
    {
        for r in results {
            if let Some(v) = r.get_mut("command") {
                redact_value(v);
            }
        }
    }
    p
}

fn redact_value(v: &mut Value) {
    if v.is_null() {
        return;
    }
    let text = match &*v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    *v = redact_text(&text);
}

/// `YYYY-MM-DD` at 00:00 UTC, or `None` for anything else or an impossible date.
pub fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d) = (
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
        it.next()?.parse::<i64>().ok()?,
    );
    if it.next().is_some() || s.len() != 10 || !(1..=12).contains(&m) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(m - 1) as usize];
    if !(1..=dim).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

/// A `/provefab <key> <disposition>[:| ]<reason>` line (spec section 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub key: String,
    pub disposition: Disposition,
    pub reason: Option<String>,
}

/// Commands of a comment body: only lines that start with `/provefab`
/// (after trimming) outside code fences. `Err(line)` for one that does not parse.
pub fn parse_commands(body: &str) -> Vec<Result<Command, String>> {
    let mut out = Vec::new();
    for (_, raw) in command_lines(body).filter(|(command, _)| *command) {
        let line = raw.trim();
        // ASCII prefix, checked by `command_lines`: slicing after it is safe.
        let rest = line[PREFIX.len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or_default().to_ascii_uppercase();
        let tail = parts.next().unwrap_or_default().trim();
        // Advance by the separator's own width: `c.is_whitespace()` matches
        // multi-byte spaces (NBSP, em space) that a `+ 1` would slice inside.
        let (word, reason) = match tail
            .char_indices()
            .find(|&(_, c)| c == ':' || c.is_whitespace())
        {
            Some((i, c)) => (&tail[..i], tail[i + c.len_utf8()..].trim()),
            None => (tail, ""),
        };
        let valid_key =
            key.len() > 1 && key.starts_with('F') && key[1..].chars().all(|c| c.is_ascii_digit());
        match (valid_key, Disposition::parse(word)) {
            (true, Some(d)) => out.push(Ok(Command {
                key,
                disposition: d,
                reason: (!reason.is_empty()).then(|| reason.to_string()),
            })),
            _ => out.push(Err(line.to_string())),
        }
    }
    out
}

/// `body` without the lines `parse_commands` treats as commands, trimmed.
pub fn strip_commands(body: &str) -> String {
    command_lines(body)
        .filter(|(command, _)| !command)
        .map(|(_, raw)| raw)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

const PREFIX: &str = "/provefab ";

/// Each line of `body`, untrimmed, with whether it is a command line: it starts
/// with `/provefab ` (any case, after trimming) outside a code fence. The one
/// rule `parse_commands` and `strip_commands` share.
fn command_lines(body: &str) -> impl Iterator<Item = (bool, &str)> {
    let mut fenced = false;
    body.lines().map(move |raw| {
        let line = raw.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            return (false, raw);
        }
        (
            !fenced && line.to_ascii_lowercase().starts_with(PREFIX),
            raw,
        )
    })
}

fn line_of(p: &Result<Command, String>) -> String {
    match p {
        Err(l) => l.clone(),
        Ok(c) => format!("/provefab {} {}", c.key, c.disposition.as_str()),
    }
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Records the `/provefab` commands of a PR's comments as human events.
    /// Idempotent: a command already recorded (same comment, same finding or
    /// line) is not recorded again, so every poll may pass the full list.
    pub async fn apply_finding_commands(
        &self,
        task: &TaskRow,
        comments: &[Comment],
    ) -> Result<(), PipelineError> {
        let keys = self.store.finding_keys(task.id).await?;
        for c in comments.iter().filter(|c| !is_bot_comment(&c.body)) {
            let comment = format!("{}@{}", c.author, c.created_at);
            let authorized = c.author == task.author
                || matches!(c.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR");
            for parsed in parse_commands(&c.body) {
                let ignored = |why: &str| Event::CommandIgnored {
                    comment: comment.clone(),
                    login: c.author.clone(),
                    line: line_of(&parsed),
                    why: why.into(),
                };
                let event = match (&parsed, authorized) {
                    (_, false) => ignored("not authorized"),
                    (Err(_), true) => ignored("not a command"),
                    (Ok(cmd), true) if !keys.contains(&cmd.key) => ignored("unknown finding"),
                    (Ok(cmd), true) => Event::FindingDisposition {
                        finding: cmd.key.clone(),
                        disposition: cmd.disposition,
                        reason: cmd.reason.clone(),
                        login: c.author.clone(),
                        association: c.association.clone(),
                        comment: comment.clone(),
                    },
                };
                self.store.record_human(task.id, &event).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_is_utc_midnight_and_rejects_impossible_dates() {
        assert_eq!(parse_date("2026-09-30"), Some(1790726400));
        assert_eq!(parse_date("2026-02-30"), None);
        assert_eq!(parse_date("x"), None);
    }

    #[test]
    fn every_kind_round_trips_with_its_source() {
        let events = [
            Event::Plan {
                pass: 1,
                summary: "s".into(),
                steps: vec![],
                risks: vec![],
            },
            Event::FindingDisposition {
                finding: "F1".into(),
                disposition: Disposition::Rejected,
                reason: None,
                login: "alice".into(),
                association: "OWNER".into(),
                comment: "alice@2026-09-30T10:00:00Z".into(),
            },
            Event::FindingInferred {
                finding: "F1".into(),
                rule: Rule::FollowedByRevert,
                rule_version: RULE_VERSION,
            },
            Event::IssueReopened { previous_pass: 1 },
        ];
        let sources = [Source::Claim, Source::Human, Source::Inferred, Source::Fact];
        for (e, s) in events.iter().zip(sources) {
            assert_eq!(e.source(), s);
            let v = serde_json::to_value(e).unwrap();
            assert_eq!(
                v["kind"],
                serde_json::Value::String(match e {
                    Event::FindingInferred { .. } => "finding_inferred".into(),
                    _ => e.kind().into(),
                })
            );
            let back = StoredEvent {
                id: 1,
                task_id: 1,
                seq: 1,
                kind: e.kind().into(),
                source: s.as_str().into(),
                schema_version: 1,
                payload: v,
                at: 0,
            };
            assert_eq!(back.typed().as_ref(), Some(e));
        }
        assert_eq!(events[2].kind(), "finding_followed_by_revert");
    }

    #[test]
    fn commands_are_whole_lines_outside_code_fences() {
        let body = "Thanks!\n/provefab F2 rejected: the value is bounded above\n/PROVEFAB f3 Fixed\nsee /provefab F9 accepted inline\n```\n/provefab F4 waived\n```\n/provefab F5 maybe\n/provefab F6 accepted   \n";
        let got = parse_commands(body);
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[0],
            Ok(Command {
                key: "F2".into(),
                disposition: Disposition::Rejected,
                reason: Some("the value is bounded above".into())
            })
        );
        assert_eq!(
            got[1],
            Ok(Command {
                key: "F3".into(),
                disposition: Disposition::Fixed,
                reason: None
            })
        );
        assert_eq!(got[2], Err("/provefab F5 maybe".into()));
        assert_eq!(
            got[3],
            Ok(Command {
                key: "F6".into(),
                disposition: Disposition::Accepted,
                reason: None
            })
        );
    }

    #[test]
    fn multi_byte_whitespace_never_panics() {
        assert_eq!(
            parse_commands("/provefab F1 rejected\u{a0}because"),
            vec![Ok(Command {
                key: "F1".into(),
                disposition: Disposition::Rejected,
                reason: Some("because".into())
            })]
        );
        assert_eq!(
            parse_commands("/provefab F1\u{2003}rejected"),
            vec![Ok(Command {
                key: "F1".into(),
                disposition: Disposition::Rejected,
                reason: None
            })]
        );
        let _ = parse_commands("/provefab \u{a0}");
        let _ = parse_commands("/provefab\u{a0}F1 rejected");
        let _ = strip_commands("/provefab \u{a0}\n\u{2003}/provefab F1 fixed");
    }

    #[test]
    fn strip_commands_removes_exactly_the_lines_parse_commands_reads() {
        let body = "a\n /PROVEFAB F1 fixed \n```\n/provefab F2 waived\n```\n/provefab F3 maybe\nsee /provefab F4 fixed\n/provefabF5 fixed";
        let stripped = strip_commands(body);
        assert_eq!(parse_commands(body).len(), 2);
        assert_eq!(
            body.lines().count() - stripped.lines().count(),
            parse_commands(body).len()
        );
        assert!(parse_commands(&stripped).is_empty());
    }

    #[test]
    fn strip_commands_removes_only_command_lines() {
        assert_eq!(
            strip_commands("Thanks!\n/provefab F1 rejected\nrename x"),
            "Thanks!\nrename x"
        );
        assert_eq!(strip_commands("/provefab F1 fixed\n"), "");
        let fenced = "see:\n```\n/provefab F4 waived\n```";
        assert_eq!(strip_commands(fenced), fenced);
    }

    fn stored(kind: &str, schema_version: u32, payload: Value) -> StoredEvent {
        StoredEvent {
            id: 1,
            task_id: 1,
            seq: 1,
            kind: kind.into(),
            source: "fact".into(),
            schema_version,
            payload,
            at: 0,
        }
    }

    #[test]
    fn an_unknown_kind_or_version_exports_no_payload() {
        let future = stored(
            "future",
            1,
            serde_json::json!({"kind": "future", "note": "SENTINEL_SECRET_42"}),
        );
        assert_eq!(redact_event(&future), serde_json::json!({"unknown": true}));
        let v2 = stored(
            "plan",
            2,
            serde_json::json!({"kind": "plan", "pass": 1, "summary": "SENTINEL_SECRET_42", "steps": [], "risks": []}),
        );
        assert_eq!(redact_event(&v2), serde_json::json!({"unknown": true}));
    }

    #[test]
    fn model_written_commands_and_error_exits_are_redacted() {
        let e = |ev: Event| stored(ev.kind(), 1, serde_json::to_value(&ev).unwrap());
        let repro = e(Event::Reproduction {
            command: "grep SENTINEL_SECRET_42 x".into(),
            failed_before_fix: true,
        });
        let out = redact_event(&repro).to_string();
        assert!(!out.contains("SENTINEL_SECRET_42"), "{out}");
        assert_eq!(redact_event(&repro)["failed_before_fix"], true);
        let gates = e(Event::GatesRun {
            stage: "gates".into(),
            round: 0,
            results: vec![GateEntry {
                command: "grep SENTINEL_SECRET_42 x".into(),
                exit: Some(1),
                timed_out: false,
                passed: false,
                output_ref: "/s".into(),
            }],
        });
        let out = redact_event(&gates);
        assert!(!out.to_string().contains("SENTINEL_SECRET_42"), "{out}");
        assert_eq!(out["results"][0]["passed"], false);
        let run = |exit: &str| {
            e(Event::StageRun {
                stage: "plan".into(),
                model_id: "m".into(),
                actual_model: None,
                provider: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
                exit: exit.into(),
            })
        };
        assert_eq!(redact_event(&run("completed"))["exit"], "completed");
        let err = redact_event(&run("provider_error: SENTINEL_SECRET_42"));
        assert!(!err.to_string().contains("SENTINEL_SECRET_42"), "{err}");
        assert_eq!(err["exit"]["redacted"], true);
    }

    #[test]
    fn an_unknown_kind_or_version_reads_as_none() {
        let e = StoredEvent {
            id: 1,
            task_id: 1,
            seq: 1,
            kind: "future".into(),
            source: "fact".into(),
            schema_version: 1,
            payload: serde_json::json!({"kind": "future"}),
            at: 0,
        };
        assert_eq!(e.typed(), None);
        let e = StoredEvent {
            schema_version: 2,
            payload: serde_json::json!({"kind": "issue_reopened", "previous_pass": 1}),
            ..e
        };
        assert_eq!(e.typed(), None);
    }
}

/// A review finding with its stable key (spec section 3.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FindingRow {
    pub id: i64,
    pub task_id: i64,
    pub key: String,
    pub pass: u32,
    pub round: u32,
    pub reviewer_model: String,
    pub severity: String,
    pub file: String,
    pub line: Option<u32>,
    pub text: String,
    /// The rule the finding cites (`R3`), checked against the rules its review was given.
    pub rule: Option<String>,
    pub event_id: i64,
}
