//! Domain types. Pure data, no IO.

use serde::{Deserialize, Serialize};

/// Model strength class. Ordered: `Fast < Standard < Frontier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Fast,
    Standard,
    Frontier,
}

impl Tier {
    /// One step stronger, saturating at `Frontier`.
    pub fn up(self) -> Self {
        match self {
            Tier::Fast => Tier::Standard,
            Tier::Standard | Tier::Frontier => Tier::Frontier,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Plan,
    Implement,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Bugfix,
    Feature,
    Refactor,
    Docs,
    Test,
    Chore,
}

impl TaskKind {
    pub const ALL: [TaskKind; 6] = [
        TaskKind::Bugfix,
        TaskKind::Feature,
        TaskKind::Refactor,
        TaskKind::Docs,
        TaskKind::Test,
        TaskKind::Chore,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Bugfix => "bugfix",
            TaskKind::Feature => "feature",
            TaskKind::Refactor => "refactor",
            TaskKind::Docs => "docs",
            TaskKind::Test => "test",
            TaskKind::Chore => "chore",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What a task works on (PR review spec section 3): an issue Provefab turns
/// into a pull request, or a pull request a person wrote that it reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskMode {
    Issue,
    PrReview,
}

impl TaskMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskMode::Issue => "issue",
            TaskMode::PrReview => "pr_review",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [TaskMode::Issue, TaskMode::PrReview]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// Where a task is in the pipeline (spec §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskState {
    Queued,
    Classified,
    Planning,
    Implementing,
    Gating,
    Reviewing,
    PrOpen,
    NeedsInfo,
    Waiting,
    NeedsYou,
    Failed,
}

impl TaskState {
    pub const ALL: [TaskState; 11] = [
        TaskState::Queued,
        TaskState::Classified,
        TaskState::Planning,
        TaskState::Implementing,
        TaskState::Gating,
        TaskState::Reviewing,
        TaskState::PrOpen,
        TaskState::NeedsInfo,
        TaskState::Waiting,
        TaskState::NeedsYou,
        TaskState::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Classified => "classified",
            TaskState::Planning => "planning",
            TaskState::Implementing => "implementing",
            TaskState::Gating => "gating",
            TaskState::Reviewing => "reviewing",
            TaskState::PrOpen => "pr_open",
            TaskState::NeedsInfo => "needs_info",
            TaskState::Waiting => "waiting",
            TaskState::NeedsYou => "needs_you",
            TaskState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// No further automatic work: the pipeline leaves the task alone.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::PrOpen | TaskState::NeedsYou | TaskState::Failed
        )
    }
}

/// Jev's classification of one task (spec §4.1).
///
/// Scores are Jev's probability-weighted 0-based level index:
/// `difficulty` in 0.0..=4.0 (5 levels), `scope` in 0.0..=3.0
/// (one file, few files, crosses modules, architectural).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub task_kind: TaskKind,
    pub difficulty: f64,
    pub difficulty_confidence: f64,
    pub scope: f64,
    pub underspecified: f64,
    /// Exact Jev version that produced this verdict, for the audit log.
    pub jev_model: String,
    /// 0 to 4: design needed to plan (D70). `None` for older verdicts.
    #[serde(default)]
    pub plan_depth: Option<f64>,
    /// 0 to 4: cost of an unnoticed subtle mistake (D70). `None` for older verdicts.
    #[serde(default)]
    pub review_risk: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_up_saturates_at_frontier() {
        assert_eq!(Tier::Fast.up(), Tier::Standard);
        assert_eq!(Tier::Standard.up(), Tier::Frontier);
        assert_eq!(Tier::Frontier.up(), Tier::Frontier);
    }

    #[test]
    fn task_kind_round_trips_through_str() {
        for k in TaskKind::ALL {
            assert_eq!(TaskKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(TaskKind::parse("epic"), None);
    }

    #[test]
    fn task_state_round_trips_through_str() {
        for s in TaskState::ALL {
            assert_eq!(TaskState::parse(s.as_str()), Some(s));
        }
        assert_eq!(TaskState::parse("done"), None);
        assert!(TaskState::PrOpen.is_terminal() && !TaskState::Waiting.is_terminal());
    }
}
