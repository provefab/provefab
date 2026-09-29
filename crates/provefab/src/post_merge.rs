//! Post-merge verification and safe rollback
//! (docs/specs/2026-09-29-post-merge-verification-design.md, revision 2).

use serde::{Deserialize, Serialize};

use crate::agents::StageRunner;
use crate::pipeline::{Pipeline, PipelineError};
use crate::ports::{Hub, Oracle};

pub const ATTRIBUTION_WAIT_SECS: i64 = 3600;
pub const INFRA_ERROR_LIMIT: i64 = 5;
pub const BASE_MOVE_LIMIT: i64 = 3;
pub const SUMMARY_MAX: usize = 2000;

/// Where a check stands (spec section 5). One transition per scheduler tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Queued,
    Verifying,
    VerificationFailed,
    PreparingRevert,
    RevertReady,
    Passed,
    Superseded,
    RevertOpen,
    Blocked,
}

impl CheckState {
    pub const ALL: [CheckState; 9] = [
        Self::Queued,
        Self::Verifying,
        Self::VerificationFailed,
        Self::PreparingRevert,
        Self::RevertReady,
        Self::Passed,
        Self::Superseded,
        Self::RevertOpen,
        Self::Blocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Verifying => "verifying",
            Self::VerificationFailed => "verification_failed",
            Self::PreparingRevert => "preparing_revert",
            Self::RevertReady => "revert_ready",
            Self::Passed => "passed",
            Self::Superseded => "superseded",
            Self::RevertOpen => "revert_open",
            Self::Blocked => "blocked",
        }
    }

    /// Unknown text is `None`: the store turns it into a corruption error,
    /// never a default state.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Superseded | Self::RevertOpen | Self::Blocked
        )
    }
}

/// Why a check needs a human, or why verification failed (spec section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    CheckFailed,
    InfraError,
    RevertConflict,
    RevertChecksFailed,
    BaseMoved,
    BaseDiverged,
    BranchConflict,
    UnsafeMergeStrategy,
    AttributionMissing,
    DirtyTree,
}

impl FailureKind {
    pub const ALL: [FailureKind; 10] = [
        Self::CheckFailed,
        Self::InfraError,
        Self::RevertConflict,
        Self::RevertChecksFailed,
        Self::BaseMoved,
        Self::BaseDiverged,
        Self::BranchConflict,
        Self::UnsafeMergeStrategy,
        Self::AttributionMissing,
        Self::DirtyTree,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckFailed => "check_failed",
            Self::InfraError => "infra_error",
            Self::RevertConflict => "revert_conflict",
            Self::RevertChecksFailed => "revert_checks_failed",
            Self::BaseMoved => "base_moved",
            Self::BaseDiverged => "base_diverged",
            Self::BranchConflict => "branch_conflict",
            Self::UnsafeMergeStrategy => "unsafe_merge_strategy",
            Self::AttributionMissing => "attribution_missing",
            Self::DirtyTree => "dirty_tree",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A command that failed twice. Only these fields may reach GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedCommand {
    pub command: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
}

/// At most `SUMMARY_MAX` characters, cut on a character boundary.
pub fn bounded(s: &str) -> String {
    if s.chars().count() <= SUMMARY_MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SUMMARY_MAX - 3).collect();
    out.push_str("...");
    out
}

impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Advances every post-merge check of a task by at most one transition.
    /// Task 5 implements the state handlers.
    pub async fn process_post_merge(&self, _task_id: i64) -> Result<(), PipelineError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_and_kinds_round_trip_and_reject_unknown_text() {
        for s in CheckState::ALL {
            assert_eq!(CheckState::parse(s.as_str()), Some(s));
        }
        for k in FailureKind::ALL {
            assert_eq!(FailureKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(CheckState::parse("failed"), None);
        assert_eq!(FailureKind::parse(""), None);
        let terminal: Vec<_> = CheckState::ALL
            .into_iter()
            .filter(|s| s.is_terminal())
            .collect();
        assert_eq!(
            terminal,
            [
                CheckState::Passed,
                CheckState::Superseded,
                CheckState::RevertOpen,
                CheckState::Blocked
            ]
        );
    }
}
