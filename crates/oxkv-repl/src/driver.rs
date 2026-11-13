//! The one replication driver interface.
//!
//! The write path sees this interface only. It owns leadership, the lease
//! term or epoch, log append, the commit index, apply, and the read barrier.
//! It does not expose a driver-specific leader concept.
//!
//! Two drivers implement the interface. The lease driver is the minimal case.
//! The Raft-like driver is the reference case. A caller cannot tell which
//! driver serves a shard from a receipt, an error, or a read.

use std::collections::BTreeSet;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
use oxkv_contracts::version::{CommitVersion, LeaseTerm};

use crate::error::ReplError;
use crate::fence::Leadership;
use crate::ids::{NodeId, Tick};
use crate::log::{EntryPayload, LogIndex};
use crate::read::ReadView;
use crate::state::ApplyReport;

/// The result of one append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    index: LogIndex,
    term: LeaseTerm,
    committed: bool,
}

impl AppendReceipt {
    /// Creates an append receipt.
    #[must_use]
    pub const fn new(index: LogIndex, term: LeaseTerm, committed: bool) -> Self {
        Self {
            index,
            term,
            committed,
        }
    }

    /// Returns the record index.
    #[must_use]
    pub const fn index(self) -> LogIndex {
        self.index
    }

    /// Returns the record term.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.term
    }

    /// Returns true when the record is already committed.
    #[must_use]
    pub const fn is_committed(self) -> bool {
        self.committed
    }
}

/// The result of one protocol advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvanceReport {
    commit_index: LogIndex,
    term: LeaseTerm,
}

impl AdvanceReport {
    /// Creates an advance report.
    #[must_use]
    pub const fn new(commit_index: LogIndex, term: LeaseTerm) -> Self {
        Self { commit_index, term }
    }

    /// Returns the commit index after the advance.
    #[must_use]
    pub const fn commit_index(self) -> LogIndex {
        self.commit_index
    }

    /// Returns the term of the advance.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.term
    }
}

/// One terminal receipt.
///
/// The state is `Committed` or `Rejected`. A terminal receipt never changes
/// state. A rejected receipt names one registry code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalReceipt {
    state: ReceiptState,
    term: LeaseTerm,
    index: Option<LogIndex>,
    commit_version: Option<CommitVersion>,
    code: Option<&'static str>,
}

impl TerminalReceipt {
    /// Creates a committed receipt.
    #[must_use]
    pub const fn committed(
        term: LeaseTerm,
        index: LogIndex,
        commit_version: CommitVersion,
    ) -> Self {
        Self {
            state: ReceiptState::Committed,
            term,
            index: Some(index),
            commit_version: Some(commit_version),
            code: None,
        }
    }

    /// Creates a rejected receipt from a named error.
    ///
    /// # Errors
    ///
    /// Returns the error unchanged when it has no `Rejected` receipt state.
    /// A code with no receipt must not close a write.
    pub fn from_rejection(term: LeaseTerm, error: ReplError) -> Result<Self, ReplError> {
        if error.receipt_state() != Some(ReceiptState::Rejected) {
            return Err(error);
        }
        Ok(Self {
            state: ReceiptState::Rejected,
            term,
            index: None,
            commit_version: None,
            code: Some(error.code()),
        })
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn state(self) -> ReceiptState {
        self.state
    }

    /// Returns the term of the receipt.
    #[must_use]
    pub const fn term(self) -> LeaseTerm {
        self.term
    }

    /// Returns the record index for a committed receipt.
    #[must_use]
    pub const fn index(self) -> Option<LogIndex> {
        self.index
    }

    /// Returns the commit version for a committed receipt.
    #[must_use]
    pub const fn commit_version(self) -> Option<CommitVersion> {
        self.commit_version
    }

    /// Returns the error code for a rejected receipt.
    #[must_use]
    pub const fn code(self) -> Option<&'static str> {
        self.code
    }

    /// Returns true when the receipt is terminal.
    ///
    /// Both receipt states in this type are terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.state.is_terminal()
    }
}

/// One replication driver.
///
/// The interface uses injected time. It does not read a clock, draw
/// randomness, or perform I/O. A record is durable when `append` returns.
pub trait ReplicationDriver {
    /// Acquires leadership and returns the fence.
    ///
    /// A driver that already holds leadership returns the same token.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::LeaseHeld`] when another holder owns the lease.
    /// Returns [`ReplError::TierUnavailable`] when no quorum is available.
    fn acquire(&mut self, now: Tick) -> Result<Leadership, ReplError>;

    /// Renews leadership.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token. Returns
    /// [`ReplError::LeaseExpired`] when the lease ended.
    fn renew(&mut self, token: &Leadership, now: Tick) -> Result<(), ReplError>;

    /// Releases leadership.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token.
    fn release(&mut self, token: &Leadership) -> Result<(), ReplError>;

    /// Appends one entry to the durable log.
    ///
    /// A repeated idempotency key returns the existing record. It does not
    /// append a second record.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token. Returns
    /// [`ReplError::LeaseExpired`] when the lease ended.
    fn append(
        &mut self,
        token: &Leadership,
        payload: EntryPayload,
        now: Tick,
    ) -> Result<AppendReceipt, ReplError>;

    /// Advances the protocol and returns the commit index.
    ///
    /// The lease driver commits at its single voter. The Raft-like driver
    /// sends messages to its peers and commits at a quorum.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] for a stale token. Returns
    /// [`ReplError::LeaseExpired`] when the leader lost its quorum.
    fn advance(&mut self, token: &Leadership, now: Tick) -> Result<AdvanceReport, ReplError>;

    /// Returns the commit index.
    fn commit_index(&self) -> LogIndex;

    /// Returns the applied index.
    fn applied_index(&self) -> LogIndex;

    /// Applies the committed log up to one index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the index is above the
    /// commit index.
    fn apply(&mut self, up_to: LogIndex) -> Result<ApplyReport, ReplError>;

    /// Rebuilds the applied state from the durable log.
    ///
    /// A restart calls this method. It is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the log has a gap.
    fn recover(&mut self) -> Result<ApplyReport, ReplError>;

    /// Serves one leader read at an applied index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::StaleEpoch`] or [`ReplError::LeaseExpired`] for a
    /// stale token. Returns [`ReplError::ReadBehindCommit`] when the index is
    /// above the applied index.
    fn read(&self, token: &Leadership, at: LogIndex, now: Tick) -> Result<ReadView, ReplError>;

    /// Serves one follower read at an applied index.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::ReadBehindCommit`] when the index is above the
    /// applied index.
    fn read_follower(&self, at: LogIndex) -> Result<ReadView, ReplError>;

    /// Returns the member set.
    fn members(&self) -> &BTreeSet<NodeId>;

    /// Returns the current leader.
    fn leader(&self) -> Option<NodeId>;

    /// Returns the current term.
    fn term(&self) -> LeaseTerm;

    /// Returns the last log index.
    fn last_index(&self) -> LogIndex;

    /// Transfers leadership to another member.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::PlacementViolation`] when the target is not a
    /// member. Returns [`ReplError::StaleEpoch`] for a stale token.
    fn transfer(
        &mut self,
        token: &Leadership,
        to: NodeId,
        now: Tick,
    ) -> Result<Leadership, ReplError>;

    /// Returns the committed receipt for one idempotency key.
    fn receipt(&self, idempotency: &IdempotencyKey) -> Option<TerminalReceipt>;
}
