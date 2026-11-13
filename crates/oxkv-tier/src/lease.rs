//! The single-writer authority lease and its fencing value.
//!
//! A shard has at most one live authority lease. Every acquisition writes a
//! strictly higher [`LeaseTerm`]. The storage boundary compares the writer term
//! with the accepted term.
//!
//! A replica cannot hold an authority lease. A local tab lease may exist for
//! multi-tab coordination, but it cannot commit data or fence authority writes.
//!
//! The module reads no clock. The caller injects [`LeaseTime`] and
//! [`LeaseDuration`].

use oxkv_contracts::version::{CommitVersion, LeaseTerm, ShardId};

use crate::error::TierError;

/// An injected logical time value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseTime(u64);

impl LeaseTime {
    /// Creates a logical time value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// An injected lease duration in logical time units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeaseDuration(u64);

impl LeaseDuration {
    /// Creates a lease duration.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the integer value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The identity of one lease holder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriterId(String);

impl WriterId {
    /// Creates a writer identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The role of one process for one shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShardRole {
    /// The process may hold an authority write lease.
    Authority,
    /// The process is read-only and cannot hold an authority write lease.
    Replica,
}

/// The fencing value that a writer presents at the storage boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaseFence {
    shard_id: ShardId,
    holder: WriterId,
    term: LeaseTerm,
}

impl LeaseFence {
    /// Creates a lease fence.
    #[must_use]
    pub const fn new(shard_id: ShardId, holder: WriterId, term: LeaseTerm) -> Self {
        Self {
            shard_id,
            holder,
            term,
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the holder identity.
    #[must_use]
    pub const fn holder(&self) -> &WriterId {
        &self.holder
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }
}

/// One durable authority lease record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    shard_id: ShardId,
    holder: WriterId,
    term: LeaseTerm,
    acquired_at: LeaseTime,
    expires_at: LeaseTime,
    last_committed_version: Option<CommitVersion>,
}

impl LeaseRecord {
    /// Creates a lease record.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        holder: WriterId,
        term: LeaseTerm,
        acquired_at: LeaseTime,
        expires_at: LeaseTime,
        last_committed_version: Option<CommitVersion>,
    ) -> Self {
        Self {
            shard_id,
            holder,
            term,
            acquired_at,
            expires_at,
            last_committed_version,
        }
    }

    /// Returns the shard identity.
    #[must_use]
    pub const fn shard_id(&self) -> &ShardId {
        &self.shard_id
    }

    /// Returns the holder identity.
    #[must_use]
    pub const fn holder(&self) -> &WriterId {
        &self.holder
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.term
    }

    /// Returns the acquisition time.
    #[must_use]
    pub const fn acquired_at(&self) -> LeaseTime {
        self.acquired_at
    }

    /// Returns the expiry time.
    #[must_use]
    pub const fn expires_at(&self) -> LeaseTime {
        self.expires_at
    }

    /// Returns the last committed version.
    #[must_use]
    pub const fn last_committed_version(&self) -> Option<CommitVersion> {
        self.last_committed_version
    }

    /// Returns true when the lease is live at the given time.
    #[must_use]
    pub const fn is_live(&self, now: LeaseTime) -> bool {
        now.get() < self.expires_at.get()
    }

    /// Returns the fence for this record.
    #[must_use]
    pub fn fence(&self) -> LeaseFence {
        LeaseFence::new(self.shard_id.clone(), self.holder.clone(), self.term)
    }
}

/// One successful authority lease acquisition or renewal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseGrant {
    record: LeaseRecord,
}

impl LeaseGrant {
    /// Returns the granted record.
    #[must_use]
    pub const fn record(&self) -> &LeaseRecord {
        &self.record
    }

    /// Returns the lease term.
    #[must_use]
    pub const fn term(&self) -> LeaseTerm {
        self.record.term
    }

    /// Returns the expiry time.
    #[must_use]
    pub const fn expires_at(&self) -> LeaseTime {
        self.record.expires_at
    }

    /// Returns the fence for this grant.
    #[must_use]
    pub fn fence(&self) -> LeaseFence {
        self.record.fence()
    }
}

/// The authority lease book for one shard.
///
/// The book tracks the highest accepted term. A term never decreases, even
/// after a release or an expiry.
#[derive(Debug, Clone)]
pub struct LeaseAuthority {
    shard_id: ShardId,
    last_term: LeaseTerm,
    record: Option<LeaseRecord>,
}

impl LeaseAuthority {
    /// Creates an empty lease book for one shard.
    #[must_use]
    pub const fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            last_term: LeaseTerm::new(0),
            record: None,
        }
    }

    /// Restores a lease book from one durable record.
    #[must_use]
    pub fn restore(shard_id: ShardId, record: Option<LeaseRecord>) -> Self {
        let last_term = record
            .as_ref()
            .map_or_else(|| LeaseTerm::new(0), LeaseRecord::term);
        Self {
            shard_id,
            last_term,
            record,
        }
    }

    /// Returns the current record.
    #[must_use]
    pub const fn record(&self) -> Option<&LeaseRecord> {
        self.record.as_ref()
    }

    /// Returns the highest accepted term.
    #[must_use]
    pub const fn last_term(&self) -> LeaseTerm {
        self.last_term
    }

    /// Acquires a new authority lease.
    ///
    /// The new term is strictly higher than every prior term.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::ReplicaAuthorityLease`] when a replica asks for an
    /// authority lease.
    /// Returns [`TierError::LeaseHeld`] when a live lease exists.
    /// Returns [`TierError::TermExhausted`] when no higher term exists.
    pub fn acquire(
        &mut self,
        role: ShardRole,
        holder: WriterId,
        now: LeaseTime,
        duration: LeaseDuration,
    ) -> Result<LeaseGrant, TierError> {
        if role == ShardRole::Replica {
            return Err(TierError::ReplicaAuthorityLease);
        }
        if let Some(record) = &self.record
            && record.is_live(now)
        {
            return Err(TierError::LeaseHeld);
        }
        let next_term = self
            .last_term
            .get()
            .checked_add(1)
            .map(LeaseTerm::new)
            .ok_or(TierError::TermExhausted)?;
        let expires_at = LeaseTime::new(now.get().saturating_add(duration.get()));
        let record = LeaseRecord::new(
            self.shard_id.clone(),
            holder,
            next_term,
            now,
            expires_at,
            self.record
                .as_ref()
                .and_then(LeaseRecord::last_committed_version),
        );
        self.last_term = next_term;
        self.record = Some(record.clone());
        Ok(LeaseGrant { record })
    }

    /// Renews the live authority lease.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::StaleEpoch`] when the fence is not the current
    /// fence.
    /// Returns [`TierError::LeaseExpired`] when the lease expired.
    pub fn renew(
        &mut self,
        fence: &LeaseFence,
        now: LeaseTime,
        duration: LeaseDuration,
    ) -> Result<LeaseGrant, TierError> {
        self.check_write(fence, now)?;
        let expires_at = LeaseTime::new(now.get().saturating_add(duration.get()));
        let Some(record) = &mut self.record else {
            return Err(TierError::LeaseExpired);
        };
        record.expires_at = expires_at;
        Ok(LeaseGrant {
            record: record.clone(),
        })
    }

    /// Releases the held authority lease.
    ///
    /// A release does not lower the accepted term.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::StaleEpoch`] when the fence is not the current
    /// fence.
    pub fn release(&mut self, fence: &LeaseFence) -> Result<(), TierError> {
        let Some(record) = &self.record else {
            return Err(TierError::StaleEpoch);
        };
        if record.term != fence.term || record.holder != fence.holder {
            return Err(TierError::StaleEpoch);
        }
        self.record = None;
        Ok(())
    }

    /// Checks a write fence at the storage boundary.
    ///
    /// The stale check runs before the expiry check. A paused writer with an
    /// old term receives [`TierError::StaleEpoch`] even after the current lease
    /// expired.
    ///
    /// # Errors
    ///
    /// Returns [`TierError::StaleEpoch`] when the fence term or holder differs
    /// from the current record.
    /// Returns [`TierError::LeaseExpired`] when the record has no live lease.
    pub fn check_write(&self, fence: &LeaseFence, now: LeaseTime) -> Result<(), TierError> {
        if fence.shard_id != self.shard_id {
            return Err(TierError::StaleEpoch);
        }
        let Some(record) = &self.record else {
            return Err(TierError::LeaseExpired);
        };
        if record.term != fence.term || record.holder != fence.holder {
            return Err(TierError::StaleEpoch);
        }
        if !record.is_live(now) {
            return Err(TierError::LeaseExpired);
        }
        Ok(())
    }

    /// Records one committed version under a valid fence.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`LeaseAuthority::check_write`].
    pub fn commit(
        &mut self,
        fence: &LeaseFence,
        now: LeaseTime,
        commit_version: CommitVersion,
    ) -> Result<(), TierError> {
        self.check_write(fence, now)?;
        let Some(record) = &mut self.record else {
            return Err(TierError::LeaseExpired);
        };
        record.last_committed_version = Some(
            record
                .last_committed_version
                .map_or(commit_version, |current| current.max(commit_version)),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use oxkv_contracts::version::{CommitVersion, LeaseTerm, ShardId};

    use super::{
        LeaseAuthority, LeaseDuration, LeaseFence, LeaseRecord, LeaseTime, ShardRole, TierError,
        WriterId,
    };

    fn authority() -> LeaseAuthority {
        LeaseAuthority::new(ShardId::new(vec![1]))
    }

    fn holder(name: &str) -> WriterId {
        WriterId::new(name)
    }

    #[test]
    fn a_new_lease_advances_the_term() {
        let mut authority = authority();
        let first = authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(10),
            )
            .expect("the first lease succeeds");
        assert_eq!(first.term(), LeaseTerm::new(1));
        authority
            .release(&first.fence())
            .expect("the holder releases the lease");
        let second = authority
            .acquire(
                ShardRole::Authority,
                holder("bob"),
                LeaseTime::new(1),
                LeaseDuration::new(10),
            )
            .expect("the second lease succeeds");
        assert_eq!(second.term(), LeaseTerm::new(2));
        assert_eq!(authority.last_term(), LeaseTerm::new(2));
    }

    #[test]
    fn a_stale_writer_is_fenced() {
        let mut authority = authority();
        let first = authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(5),
            )
            .expect("the first lease succeeds");
        let second = authority
            .acquire(
                ShardRole::Authority,
                holder("bob"),
                LeaseTime::new(5),
                LeaseDuration::new(5),
            )
            .expect("the second lease succeeds");
        assert_eq!(
            authority.check_write(&first.fence(), LeaseTime::new(6)),
            Err(TierError::StaleEpoch)
        );
        assert_eq!(
            authority.check_write(&second.fence(), LeaseTime::new(6)),
            Ok(())
        );
    }

    #[test]
    fn an_expired_lease_is_named() {
        let mut authority = authority();
        let grant = authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(5),
            )
            .expect("the first lease succeeds");
        assert_eq!(
            authority.check_write(&grant.fence(), LeaseTime::new(5)),
            Err(TierError::LeaseExpired)
        );
    }

    #[test]
    fn a_held_lease_refuses_a_second_writer() {
        let mut authority = authority();
        authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(5),
            )
            .expect("the first lease succeeds");
        assert_eq!(
            authority.acquire(
                ShardRole::Authority,
                holder("bob"),
                LeaseTime::new(1),
                LeaseDuration::new(5),
            ),
            Err(TierError::LeaseHeld)
        );
    }

    #[test]
    fn a_replica_cannot_hold_an_authority_lease() {
        let mut authority = authority();
        assert_eq!(
            authority.acquire(
                ShardRole::Replica,
                holder("replica-a"),
                LeaseTime::new(0),
                LeaseDuration::new(5),
            ),
            Err(TierError::ReplicaAuthorityLease)
        );
        assert_eq!(authority.last_term(), LeaseTerm::new(0));
    }

    #[test]
    fn the_commit_record_keeps_the_highest_version() {
        let mut authority = authority();
        let grant = authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(10),
            )
            .expect("the lease succeeds");
        authority
            .commit(&grant.fence(), LeaseTime::new(1), CommitVersion::new(9))
            .expect("the commit is valid");
        authority
            .commit(&grant.fence(), LeaseTime::new(2), CommitVersion::new(7))
            .expect("the commit is valid");
        assert_eq!(
            authority
                .record()
                .and_then(LeaseRecord::last_committed_version)
                .map(CommitVersion::get),
            Some(9)
        );
    }

    #[test]
    fn a_restored_record_keeps_the_fence_floor() {
        let mut authority = authority();
        let grant = authority
            .acquire(
                ShardRole::Authority,
                holder("alice"),
                LeaseTime::new(0),
                LeaseDuration::new(10),
            )
            .expect("the lease succeeds");
        let restored = LeaseAuthority::restore(ShardId::new(vec![1]), Some(grant.record().clone()));
        assert_eq!(restored.last_term(), LeaseTerm::new(1));
        assert_eq!(
            restored.check_write(&grant.fence(), LeaseTime::new(9)),
            Ok(())
        );
    }

    #[test]
    fn a_fence_for_another_shard_is_stale() {
        let authority = authority();
        let fence = LeaseFence::new(ShardId::new(vec![2]), holder("alice"), LeaseTerm::new(1));
        assert_eq!(
            authority.check_write(&fence, LeaseTime::new(0)),
            Err(TierError::StaleEpoch)
        );
    }
}
