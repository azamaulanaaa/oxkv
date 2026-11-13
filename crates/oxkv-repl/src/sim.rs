//! Deterministic fault injection for the leader-kill test.
//!
//! The harness kills the leader at one named phase. It restarts the authority
//! and retries the write with the same idempotency key. Every run must end
//! with one terminal receipt and one committed state.

use core::fmt;

use crate::driver::{ReplicationDriver, TerminalReceipt};
use crate::error::ReplError;
use crate::ids::Tick;
use crate::lease_driver::LeaseDriver;
use crate::log::LogIndex;
use crate::raft::RaftDriver;
use crate::read::KeyValue;
use crate::write::{WriteAttempt, WritePhase, WriteRequest};

/// One phase boundary where the harness kills the leader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillPoint {
    /// Kill after leadership is acquired and before the append.
    BeforeAppend,
    /// Kill after the record is durable and before the commit.
    AfterAppendBeforeCommit,
    /// Kill after the commit and before the apply.
    AfterCommitBeforeApply,
    /// Kill after the apply and before the reply.
    AfterApplyBeforeReply,
}

impl KillPoint {
    /// Every kill point.
    pub const ALL: [Self; 4] = [
        Self::BeforeAppend,
        Self::AfterAppendBeforeCommit,
        Self::AfterCommitBeforeApply,
        Self::AfterApplyBeforeReply,
    ];

    /// Returns the phase that the write reaches at this kill point.
    #[must_use]
    pub const fn phase(self) -> WritePhase {
        match self {
            Self::BeforeAppend => WritePhase::Start,
            Self::AfterAppendBeforeCommit => WritePhase::Appended,
            Self::AfterCommitBeforeApply => WritePhase::Committed,
            Self::AfterApplyBeforeReply => WritePhase::Applied,
        }
    }

    /// Returns the count of steps before the kill.
    #[must_use]
    pub const fn steps(self) -> usize {
        match self {
            Self::BeforeAppend => 0,
            Self::AfterAppendBeforeCommit => 1,
            Self::AfterCommitBeforeApply => 2,
            Self::AfterApplyBeforeReply => 3,
        }
    }
}

impl fmt::Display for KillPoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BeforeAppend => "before-append",
            Self::AfterAppendBeforeCommit => "after-append-before-commit",
            Self::AfterCommitBeforeApply => "after-commit-before-apply",
            Self::AfterApplyBeforeReply => "after-apply-before-reply",
        })
    }
}

/// A driver that can lose a leader and restart.
pub trait CrashableDriver: ReplicationDriver {
    /// Kills the current leader.
    ///
    /// # Errors
    ///
    /// Returns a named error when the durable state cannot continue.
    fn crash_leader(&mut self) -> Result<(), ReplError>;

    /// Restarts the authority and rebuilds the applied state.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the committed log has a
    /// gap.
    fn restart(&mut self) -> Result<(), ReplError>;
}

impl CrashableDriver for LeaseDriver {
    fn crash_leader(&mut self) -> Result<(), ReplError> {
        LeaseDriver::crash_leader(self)
    }

    fn restart(&mut self) -> Result<(), ReplError> {
        LeaseDriver::restart(self).map(|_| ())
    }
}

impl CrashableDriver for RaftDriver {
    fn crash_leader(&mut self) -> Result<(), ReplError> {
        RaftDriver::crash_leader(self);
        Ok(())
    }

    fn restart(&mut self) -> Result<(), ReplError> {
        RaftDriver::restart(self).map(|_| ())
    }
}

/// The result of one leader-kill run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillOutcome {
    /// The phase where the harness killed the leader.
    pub point: KillPoint,
    /// The one terminal receipt.
    pub receipt: TerminalReceipt,
    /// The commit index after the retry.
    pub commit_index: LogIndex,
    /// The applied index after the retry.
    pub applied_index: LogIndex,
    /// The applied state after the retry.
    pub state: Vec<KeyValue>,
}

impl KillOutcome {
    /// Returns the value for one key.
    #[must_use]
    pub fn value(&self, key: &[u8]) -> Option<&[u8]> {
        self.state
            .iter()
            .find(|record| record.key == key)
            .map(|record| record.value.as_slice())
    }
}

/// Kills the leader at one phase, restarts it, and retries the write.
///
/// # Errors
///
/// Returns [`ReplError::InvariantViolation`] when a phase does not match the
/// kill point. Returns a retryable driver error when the retry cannot reach a
/// terminal receipt.
pub fn run_kill_point<D: CrashableDriver>(
    driver: &mut D,
    request: &WriteRequest,
    point: KillPoint,
    now: Tick,
) -> Result<KillOutcome, ReplError> {
    let mut attempt = WriteAttempt::new(request.clone());
    attempt.start(driver, now)?;
    for _ in 0..point.steps() {
        if attempt.phase() == WritePhase::Terminal {
            break;
        }
        attempt.step(driver, now)?;
    }
    if attempt.phase() != point.phase() {
        return Err(ReplError::InvariantViolation {
            reason: "the write phase does not match the kill point",
        });
    }
    driver.crash_leader()?;
    driver.restart()?;
    let mut retry = WriteAttempt::new(request.clone());
    let receipt = retry.run(driver, now)?;
    let applied = driver.applied_index();
    let state = driver.read_follower(applied)?.records().to_vec();
    Ok(KillOutcome {
        point,
        receipt,
        commit_index: driver.commit_index(),
        applied_index: applied,
        state,
    })
}
