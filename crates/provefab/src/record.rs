//! The evidence and decision record
//! (docs/specs/2026-09-30-evidence-record-design.md): what Provefab observed,
//! what models claimed, what humans decided and what Provefab inferred.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    const PREFIX: &str = "/provefab ";
    let mut out = Vec::new();
    let mut fenced = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !line.to_ascii_lowercase().starts_with(PREFIX) {
            continue;
        }
        let rest = line[PREFIX.len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or_default().to_ascii_uppercase();
        let tail = parts.next().unwrap_or_default().trim();
        let (word, reason) = match tail.find(|c: char| c == ':' || c.is_whitespace()) {
            Some(i) => (&tail[..i], tail[i + 1..].trim()),
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
    let mut fenced = false;
    let mut kept = Vec::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && line.to_ascii_lowercase().starts_with("/provefab ") {
            continue;
        }
        kept.push(raw);
    }
    kept.join("\n").trim().to_string()
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
    fn strip_commands_removes_only_command_lines() {
        assert_eq!(
            strip_commands("Thanks!\n/provefab F1 rejected\nrename x"),
            "Thanks!\nrename x"
        );
        assert_eq!(strip_commands("/provefab F1 fixed\n"), "");
        let fenced = "see:\n```\n/provefab F4 waived\n```";
        assert_eq!(strip_commands(fenced), fenced);
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
    pub event_id: i64,
}
