//! The lease driver.
//!
//! The lease driver is the minimal implementation of the seam. It has one
//! voter. A write commits when the append returns. A backup accepts a new
//! primary only after the lease expires.
//!
//! A crash discards the uncommitted tail. The commit index stays durable. A
//! restart rebuilds the applied state from the committed prefix.

use std::collections::BTreeSet;

use oxkv_contracts::receipt::IdempotencyKey;
use oxkv_contracts::version::{CommitVersion, LeaseTerm};

use crate::driver::{AdvanceReport, AppendReceipt, ReplicationDriver, TerminalReceipt};
use crate::error::ReplError;
use crate::fence::Leadership;
use crate::ids::{NodeId, Tick};
use crate::lease::{LeaseDuration, LeaseKeeper};
use crate::log::{EntryPayload, Log, LogIndex, OwnerFence};
use crate::read::ReadView;
use crate::state::{ApplyReport, KvState};

/// A primary-backup driver with a time-bounded lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseDriver {
    node: NodeId,
    lease: LeaseKeeper,
    log: Log,
    commit: LogIndex,
    state: KvState,
    members: BTreeSet<NodeId>,
    crashed: bool,
}

impl LeaseDriver {
    /// Creates a lease driver.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::PlacementViolation`] when the node is not a member
    /// or the member set is empty.
    pub fn new(
        node: NodeId,
        members: impl IntoIterator<Item = NodeId>,
        duration: LeaseDuration,
    ) -> Result<Self, ReplError> {
        let members = members.into_iter().collect::<BTreeSet<_>>();
        if members.is_empty() || !members.contains(&node) {
            return Err(ReplError::PlacementViolation {
                reason: "the authority node is not a member",
            });
        }
        Ok(Self {
            node,
            lease: LeaseKeeper::new(duration),
            log: Log::new(),
            commit: LogIndex::ZERO,
            state: KvState::new(),
            members,
            crashed: false,
        })
    }

    /// Returns the authority node.
    #[must_use]
    pub const fn node(&self) -> NodeId {
        self.node
    }

    /// Kills the authority.
    ///
    /// The durable log keeps the committed prefix. The method drops the lease,
    /// the uncommitted tail, and the applied state.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the commit index would
    /// overflow.
    pub fn crash_leader(&mut self) -> Result<(), ReplError> {
        self.lease.invalidate();
        let keep = self.commit.next()?;
        self.log.truncate_from(keep)?;
        self.state.reset();
        self.crashed = true;
        Ok(())
    }

    /// Restarts the authority.
    ///
    /// # Errors
    ///
    /// Returns [`ReplError::InvariantViolation`] when the log has a gap.
    pub fn restart(&mut self) -> Result<ApplyReport, ReplError> {
        self.crashed = false;
        self.recover()
    }

    fn ensure_running(&self) -> Result<(), ReplError> {
        if self.crashed {
            return Err(ReplError::InvariantViolation {
                reason: "the authority is not running",
            });
        }
        Ok(())
    }

    fn check(&self, token: &Leadership, now: Tick) -> Result<(), ReplError> {
        self.ensure_running()?;
        self.lease.validate_at(token, now)
    }
}

impl ReplicationDriver for LeaseDriver {
    fn acquire(&mut self, now: Tick) -> Result<Leadership, ReplError> {
        self.ensure_running()?;
        if let Some(lease) = self.lease.lease()
            && lease.holder() == self.node
            && now < lease.expires_at()
        {
            return Ok(Leadership::new(self.node, self.lease.current_fence()));
        }
        self.lease.acquire(self.node, now)
    }

    fn renew(&mut self, token: &Leadership, now: Tick) -> Result<(), ReplError> {
        self.ensure_running()?;
        self.lease.renew(token, now)
    }

    fn release(&mut self, token: &Leadership) -> Result<(), ReplError> {
        self.ensure_running()?;
        self.lease.release(token)
    }

    fn append(
        &mut self,
        token: &Leadership,
        payload: EntryPayload,
        now: Tick,
    ) -> Result<AppendReceipt, ReplError> {
        self.check(token, now)?;
        if let Some(idempotency) = payload.idempotency()
            && let Some(record) = self.log.find_idempotency(idempotency)
        {
            return Ok(AppendReceipt::new(
                record.index(),
                record.term(),
                record.index() <= self.commit,
            ));
        }
        let record = self
            .log
            .append(token.term(), OwnerFence::new(token.term()), payload)?;
        Ok(AppendReceipt::new(
            record.index(),
            record.term(),
            record.index() <= self.commit,
        ))
    }

    fn advance(&mut self, token: &Leadership, now: Tick) -> Result<AdvanceReport, ReplError> {
        self.check(token, now)?;
        let last = self.log.last_index();
        if last > self.commit {
            self.commit = last;
        }
        Ok(AdvanceReport::new(self.commit, token.term()))
    }

    fn commit_index(&self) -> LogIndex {
        self.commit
    }

    fn applied_index(&self) -> LogIndex {
        self.state.applied_index()
    }

    fn apply(&mut self, up_to: LogIndex) -> Result<ApplyReport, ReplError> {
        if up_to > self.commit {
            return Err(ReplError::InvariantViolation {
                reason: "the apply index is above the commit index",
            });
        }
        self.state.apply(&self.log, up_to)
    }

    fn recover(&mut self) -> Result<ApplyReport, ReplError> {
        self.state.reset();
        self.state.apply(&self.log, self.commit)
    }

    fn read(&self, token: &Leadership, at: LogIndex, now: Tick) -> Result<ReadView, ReplError> {
        self.check(token, now)?;
        self.state.read_at(at)
    }

    fn read_follower(&self, at: LogIndex) -> Result<ReadView, ReplError> {
        self.state.read_at(at)
    }

    fn members(&self) -> &BTreeSet<NodeId> {
        &self.members
    }

    fn leader(&self) -> Option<NodeId> {
        self.lease.lease().map(|_| self.node)
    }

    fn term(&self) -> LeaseTerm {
        self.lease.current_fence().term()
    }

    fn last_index(&self) -> LogIndex {
        self.log.last_index()
    }

    fn transfer(
        &mut self,
        token: &Leadership,
        to: NodeId,
        now: Tick,
    ) -> Result<Leadership, ReplError> {
        self.check(token, now)?;
        if !self.members.contains(&to) {
            return Err(ReplError::PlacementViolation {
                reason: "the transfer target is not a member",
            });
        }
        if to == self.node {
            return Ok(*token);
        }
        self.lease.release(token)?;
        self.node = to;
        self.lease.acquire(to, now)
    }

    fn receipt(&self, idempotency: &IdempotencyKey) -> Option<TerminalReceipt> {
        if let Some(record) = self.log.find_idempotency(idempotency)
            && record.index() <= self.commit
        {
            let version = self.log.commit_version_at(record.index());
            return Some(TerminalReceipt::committed(
                record.term(),
                record.index(),
                CommitVersion::new(version),
            ));
        }
        self.state.receipt_for(idempotency)
    }
}
