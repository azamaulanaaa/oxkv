//! Authority receipts, scheduler drain, and client outbox reconciliation.
//!
//! A migration ends every authority-known receipt in a terminal state. A
//! client local outbox entry is not an authority receipt. A migration never
//! ends an entry that the authority never saw.

use std::collections::BTreeSet;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptState};
use oxkv_contracts::version::{
    CommitVersion, LeaseTerm, ManifestDigest, ScopeVersionVector, ShardId,
};

use crate::error::OpsError;

/// One receipt that the authority knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityReceiptRecord {
    /// The idempotency key that identifies the write.
    pub idempotency_key: IdempotencyKey,
    /// The receipt state.
    pub state: ReceiptState,
    /// The shard commit version of a `Committed` receipt.
    pub commit_version: Option<CommitVersion>,
    /// The named reason of a `Rejected` receipt.
    pub reason_code: Option<&'static str>,
}

impl AuthorityReceiptRecord {
    /// Creates an authority receipt record.
    #[must_use]
    pub const fn new(
        idempotency_key: IdempotencyKey,
        state: ReceiptState,
        commit_version: Option<CommitVersion>,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            idempotency_key,
            state,
            commit_version,
            reason_code,
        }
    }

    /// Returns true when the receipt is terminal.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

/// One write that waits in the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWrite {
    /// The idempotency key that identifies the write.
    pub idempotency_key: IdempotencyKey,
    /// The commit version when the scheduler can commit the write.
    pub commit_version: Option<CommitVersion>,
}

impl PendingWrite {
    /// Creates a pending write.
    #[must_use]
    pub const fn new(
        idempotency_key: IdempotencyKey,
        commit_version: Option<CommitVersion>,
    ) -> Self {
        Self {
            idempotency_key,
            commit_version,
        }
    }
}

/// One client local outbox entry.
///
/// The authority never saw this entry. A migration must not end it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOutboxEntry {
    /// The idempotency key that the client holds.
    pub idempotency_key: IdempotencyKey,
    /// The epoch when the client created the entry.
    pub created_epoch: LeaseTerm,
}

impl ClientOutboxEntry {
    /// Creates a client local outbox entry.
    #[must_use]
    pub const fn new(idempotency_key: IdempotencyKey, created_epoch: LeaseTerm) -> Self {
        Self {
            idempotency_key,
            created_epoch,
        }
    }

    /// Checks this entry after one migration.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::ClientOutboxStale`] when the entry is older than the
    /// current epoch. The error carries the rebase path.
    pub fn reconnect(&self, current_epoch: LeaseTerm, rebase: RebasePath) -> Result<(), OpsError> {
        if self.created_epoch < current_epoch {
            return Err(OpsError::ClientOutboxStale { rebase });
        }
        Ok(())
    }
}

/// The result of a scheduler drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainReport {
    /// Every authority receipt after the drain.
    pub terminal: Vec<AuthorityReceiptRecord>,
    /// Every client local entry that the authority never saw.
    pub untouched_client: Vec<ClientOutboxEntry>,
}

impl DrainReport {
    /// Returns true when every authority receipt is terminal.
    #[must_use]
    pub fn all_terminal(&self) -> bool {
        self.terminal
            .iter()
            .all(AuthorityReceiptRecord::is_terminal)
    }

    /// Returns true when the authority knows the idempotency key.
    #[must_use]
    pub fn knows(&self, key: &IdempotencyKey) -> bool {
        self.terminal
            .iter()
            .any(|record| &record.idempotency_key == key)
    }
}

/// The scheduler queue and its terminal receipts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerState {
    /// Writes that wait for the scheduler.
    pub queued: Vec<PendingWrite>,
    /// Writes that are in flight.
    pub in_flight: Vec<PendingWrite>,
    /// Receipts that are already terminal.
    pub terminal: Vec<AuthorityReceiptRecord>,
    /// Client local entries that the authority never saw.
    pub client_local: Vec<ClientOutboxEntry>,
}

impl SchedulerState {
    /// Creates an empty scheduler state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one write that will commit.
    pub fn queue_commit(&mut self, key: IdempotencyKey, commit_version: CommitVersion) {
        self.queued
            .push(PendingWrite::new(key, Some(commit_version)));
    }

    /// Queues one write that the migration will reject.
    pub fn queue_reject(&mut self, key: IdempotencyKey) {
        self.queued.push(PendingWrite::new(key, None));
    }

    /// Adds one terminal receipt.
    pub fn add_terminal(&mut self, receipt: AuthorityReceiptRecord) {
        self.terminal.push(receipt);
    }

    /// Adds one client local entry.
    pub fn add_client_local(&mut self, entry: ClientOutboxEntry) {
        self.client_local.push(entry);
    }

    /// Drains every pending write into a terminal receipt.
    ///
    /// A pending write with a commit version becomes `Committed`. A pending
    /// write without a commit version becomes `Rejected` with the maintenance
    /// reason. Client local entries stay untouched.
    ///
    /// # Errors
    ///
    /// Returns [`OpsError::MigrationIncomplete`] when an authority receipt is
    /// still `Queued` after the drain.
    pub fn drain(&mut self) -> Result<DrainReport, OpsError> {
        let pending = self
            .queued
            .drain(..)
            .chain(self.in_flight.drain(..))
            .collect::<Vec<_>>();
        let mut closed = Vec::with_capacity(pending.len() + self.terminal.len());
        for write in pending {
            let record = match write.commit_version {
                Some(commit_version) => AuthorityReceiptRecord::new(
                    write.idempotency_key,
                    ReceiptState::Committed,
                    Some(commit_version),
                    None,
                ),
                None => AuthorityReceiptRecord::new(
                    write.idempotency_key,
                    ReceiptState::Rejected,
                    None,
                    Some("shard.frozen_for_maintenance"),
                ),
            };
            closed.push(record);
        }
        closed.extend(std::mem::take(&mut self.terminal));
        self.terminal = closed;
        if self.terminal.iter().any(|record| !record.is_terminal()) {
            return Err(OpsError::MigrationIncomplete {
                step: "receipt_drain",
            });
        }
        Ok(DrainReport {
            terminal: self.terminal.clone(),
            untouched_client: self.client_local.clone(),
        })
    }

    /// Returns the number of known idempotency keys.
    #[must_use]
    pub fn known_keys(&self) -> BTreeSet<&IdempotencyKey> {
        self.terminal
            .iter()
            .map(|record| &record.idempotency_key)
            .collect()
    }
}

/// The action that a client must take to rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebaseAction {
    /// Rebase the entry on the latest committed state.
    RebaseOnLatest,
    /// Discard the local state and run a full resync.
    FullResync,
}

impl RebaseAction {
    /// Returns the canonical action name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RebaseOnLatest => "rebase_on_latest",
            Self::FullResync => "full_resync",
        }
    }
}

impl core::fmt::Display for RebaseAction {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The path that a stale client must follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePath {
    /// The manifest digest at the migration target.
    pub manifest_digest: ManifestDigest,
    /// The target version vector.
    pub version_vector: ScopeVersionVector,
    /// The rebase action.
    pub action: RebaseAction,
}

impl RebasePath {
    /// Creates a rebase path.
    #[must_use]
    pub const fn new(
        manifest_digest: ManifestDigest,
        version_vector: ScopeVersionVector,
        action: RebaseAction,
    ) -> Self {
        Self {
            manifest_digest,
            version_vector,
            action,
        }
    }
}

impl core::fmt::Display for RebasePath {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "manifest {} with action {}",
            self.manifest_digest, self.action
        )
    }
}

/// One named rebase path for a shard.
#[must_use]
pub fn rebase_path(
    manifest_digest: ManifestDigest,
    shard_id: ShardId,
    commit_version: CommitVersion,
    action: RebaseAction,
) -> RebasePath {
    let vector = ScopeVersionVector::from_vector(
        oxkv_contracts::version::VersionVector::from_entries([(shard_id, commit_version)]),
    );
    RebasePath::new(manifest_digest, vector, action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: u8) -> IdempotencyKey {
        IdempotencyKey::new(vec![value])
    }

    #[test]
    fn a_drain_ends_every_authority_receipt() {
        let mut state = SchedulerState::new();
        state.queue_commit(key(1), CommitVersion::new(7));
        state.queue_reject(key(2));
        state.add_terminal(AuthorityReceiptRecord::new(
            key(3),
            ReceiptState::Conflict,
            None,
            Some("write.version_conflict"),
        ));
        let report = state.drain().expect("the drain succeeds");
        assert!(report.all_terminal());
        assert_eq!(report.terminal.len(), 3);
        assert_eq!(report.terminal[0].state, ReceiptState::Committed);
        assert_eq!(report.terminal[1].state, ReceiptState::Rejected);
        assert_eq!(
            report.terminal[1].reason_code,
            Some("shard.frozen_for_maintenance")
        );
        assert_eq!(report.terminal[2].state, ReceiptState::Conflict);
    }

    #[test]
    fn a_drain_does_not_end_a_client_local_entry() {
        let mut state = SchedulerState::new();
        state.add_client_local(ClientOutboxEntry::new(key(9), LeaseTerm::new(1)));
        let report = state.drain().expect("the drain succeeds");
        assert_eq!(report.untouched_client.len(), 1);
        assert!(!report.knows(&key(9)));
        assert!(
            state
                .client_local
                .iter()
                .any(|entry| entry.idempotency_key == key(9))
        );
    }

    #[test]
    fn an_old_client_entry_receives_a_rebase_path() {
        let entry = ClientOutboxEntry::new(key(5), LeaseTerm::new(1));
        let rebase = rebase_path(
            ManifestDigest::from_bytes([2; 32]),
            ShardId::new(vec![1]),
            CommitVersion::new(8),
            RebaseAction::RebaseOnLatest,
        );
        let error = entry
            .reconnect(LeaseTerm::new(2), rebase.clone())
            .expect_err("the entry is stale");
        assert_eq!(error.code(), "ops.client_outbox_stale");
        assert_eq!(
            error,
            OpsError::ClientOutboxStale {
                rebase: rebase.clone()
            }
        );
        assert!(error.to_string().contains("rebase_on_latest"));
        assert_eq!(entry.reconnect(LeaseTerm::new(1), rebase), Ok(()));
    }
}
