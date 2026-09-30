//! The evidence and decision record
//! (docs/specs/2026-09-30-evidence-record-design.md): what Provefab observed,
//! what models claimed, what humans decided and what Provefab inferred.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
