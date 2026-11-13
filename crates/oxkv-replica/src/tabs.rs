//! The local tab lease and the single cursor advancer.
//!
//! One replica identity owns one mirror and one cursor. One tab advances the
//! cursor at a time. Other tabs read through the leader tab.
//!
//! The local tab lease does not commit data. It does not fence an authority
//! write. A replica holds no authority write lease.

use crate::error::ReplicaError;

/// The identity of one replica origin and its storage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplicaIdentity(Vec<u8>);

impl ReplicaIdentity {
    /// Creates a replica identity.
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// The identity of one browser tab or worker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(String);

impl TabId {
    /// Creates a tab identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the tab identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The guard that one tab holds while it advances the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdvancer {
    replica_id: ReplicaIdentity,
    tab_id: TabId,
    epoch: u64,
}

impl CursorAdvancer {
    /// Returns the replica identity.
    #[must_use]
    pub const fn replica_id(&self) -> &ReplicaIdentity {
        &self.replica_id
    }

    /// Returns the tab identity.
    #[must_use]
    pub const fn tab_id(&self) -> &TabId {
        &self.tab_id
    }

    /// Returns the lease epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// The local tab lease.
#[derive(Debug, Clone)]
pub struct LocalTabLease {
    replica_id: ReplicaIdentity,
    holder: Option<TabId>,
    epoch: u64,
}

impl LocalTabLease {
    /// Creates an unheld local tab lease.
    #[must_use]
    pub const fn new(replica_id: ReplicaIdentity) -> Self {
        Self {
            replica_id,
            holder: None,
            epoch: 0,
        }
    }

    /// Acquires the lease for one tab.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::TabLeaseHeld`] when another tab holds the lease.
    pub fn acquire(&mut self, tab: TabId) -> Result<CursorAdvancer, ReplicaError> {
        if self.holder.is_some() {
            return Err(ReplicaError::TabLeaseHeld);
        }
        self.epoch = self.epoch.saturating_add(1);
        self.holder = Some(tab.clone());
        Ok(CursorAdvancer {
            replica_id: self.replica_id.clone(),
            tab_id: tab,
            epoch: self.epoch,
        })
    }

    /// Releases the lease for one tab.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError::StaleTabLease`] when the guard does not match the
    /// current holder or epoch.
    pub fn release(&mut self, advancer: &CursorAdvancer) -> Result<(), ReplicaError> {
        if advancer.replica_id != self.replica_id
            || self.holder.as_ref() != Some(&advancer.tab_id)
            || self.epoch != advancer.epoch
        {
            return Err(ReplicaError::StaleTabLease {
                expected: self.epoch,
                found: advancer.epoch,
            });
        }
        self.holder = None;
        Ok(())
    }

    /// Returns the current holder.
    #[must_use]
    pub const fn holder(&self) -> Option<&TabId> {
        self.holder.as_ref()
    }

    /// Returns the current lease epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns true when the lease commits data. It is always false.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn commits_data(&self) -> bool {
        false
    }

    /// Returns true when the lease fences authority writes. It is always false.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn fences_authority_writes(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalTabLease, ReplicaIdentity, TabId};

    #[test]
    fn one_tab_holds_the_lease_at_a_time() {
        let mut lease = LocalTabLease::new(ReplicaIdentity::new(b"origin".to_vec()));
        let first = lease
            .acquire(TabId::new("tab-a"))
            .expect("the lease is free");
        assert_eq!(
            lease
                .acquire(TabId::new("tab-b"))
                .expect_err("the lease is held")
                .code(),
            "replica.tab_lease_held"
        );
        lease.release(&first).expect("the guard matches");
        assert!(lease.acquire(TabId::new("tab-b")).is_ok());
    }

    #[test]
    fn a_stale_guard_cannot_release_the_lease() {
        let mut lease = LocalTabLease::new(ReplicaIdentity::new(b"origin".to_vec()));
        let first = lease
            .acquire(TabId::new("tab-a"))
            .expect("the lease is free");
        lease.release(&first).expect("the guard matches");
        assert_eq!(
            lease
                .release(&first)
                .expect_err("the guard is stale")
                .code(),
            "replica.stale_tab_lease"
        );
    }

    #[test]
    fn the_local_lease_never_writes_or_fences_authority() {
        let lease = LocalTabLease::new(ReplicaIdentity::new(b"origin".to_vec()));
        assert!(!lease.commits_data());
        assert!(!lease.fences_authority_writes());
    }
}
