//! Replica coordination without an authority lease.
//!
//! A replica is read-only. It must not hold an authority write lease. A
//! [`LocalTabLease`] may coordinate tabs, but it cannot commit data and it
//! cannot fence an authority write.

/// The identity of one replica.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplicaId(String);

impl ReplicaId {
    /// Creates a replica identity.
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

/// A local tab lease for one replica.
///
/// One cursor advancer exists per replica identity. A follower tab reads
/// through the shared storage or the leader. The local tab lease only carries
/// tab coordination state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalTabLease {
    replica_id: ReplicaId,
    tab_id: String,
    generation: u64,
}

#[allow(clippy::unused_self)]
impl LocalTabLease {
    /// Creates a local tab lease.
    #[must_use]
    pub fn new(replica_id: ReplicaId, tab_id: impl Into<String>, generation: u64) -> Self {
        Self {
            replica_id,
            tab_id: tab_id.into(),
            generation,
        }
    }

    /// Returns the replica identity.
    #[must_use]
    pub const fn replica_id(&self) -> &ReplicaId {
        &self.replica_id
    }

    /// Returns the tab identity.
    #[must_use]
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    /// Returns the tab generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns false. A local tab lease is not an authority write lease.
    #[must_use]
    pub const fn holds_authority_write_lease(&self) -> bool {
        false
    }

    /// Returns false. A local tab lease cannot commit data.
    #[must_use]
    pub const fn may_commit_data(&self) -> bool {
        false
    }

    /// Returns false. A local tab lease cannot fence an authority write.
    #[must_use]
    pub const fn may_fence_authority_writes(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalTabLease, ReplicaId};

    #[test]
    fn a_local_tab_lease_is_not_an_authority_lease() {
        let lease = LocalTabLease::new(ReplicaId::new("replica-a"), "leader-tab", 3);
        assert!(!lease.holds_authority_write_lease());
        assert!(!lease.may_commit_data());
        assert!(!lease.may_fence_authority_writes());
        assert_eq!(lease.replica_id().as_str(), "replica-a");
        assert_eq!(lease.tab_id(), "leader-tab");
        assert_eq!(lease.generation(), 3);
    }
}
