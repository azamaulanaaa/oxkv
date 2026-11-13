//! Outbox entries and local dispositions.
//!
//! An [`OutboxEntry`] holds one intent before submission. It carries the
//! idempotency key, the request body digest, the target shard, the base
//! versions, and the local disposition.
//!
//! A local disposition is not a receipt. Only the authority writes a receipt
//! verdict about data.

use core::fmt;

use oxkv_contracts::receipt::{IdempotencyKey, ReceiptOutcome, RequestId};
use oxkv_contracts::version::{Hash32, KeyVersion, ShardId};

/// A digest of one request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestDigest(Hash32);

impl RequestDigest {
    /// Computes the digest of request body bytes.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(Hash32::of(bytes))
    }

    /// Creates a digest from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Returns the lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for RequestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// One base version for one mutated key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BaseVersion {
    key: Vec<u8>,
    base_version: KeyVersion,
}

impl BaseVersion {
    /// Creates a base version pair.
    #[must_use]
    pub const fn new(key: Vec<u8>, base_version: KeyVersion) -> Self {
        Self { key, base_version }
    }

    /// Returns the mutated key.
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// Returns the base version of the key.
    #[must_use]
    pub const fn base_version(&self) -> KeyVersion {
        self.base_version
    }
}

/// The local state of one outbox entry.
///
/// A local state is not a receipt state. A local cancel never appears in a
/// receipt vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LocalDisposition {
    /// The entry waits for a send or a verdict.
    Queued,
    /// The client asked to cancel the entry.
    CancelRequested,
    /// The client closed the entry without an authority verdict.
    Abandoned,
}

impl LocalDisposition {
    /// Every local disposition.
    pub const ALL: [Self; 3] = [Self::Queued, Self::CancelRequested, Self::Abandoned];

    /// Returns the canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::CancelRequested => "CancelRequested",
            Self::Abandoned => "Abandoned",
        }
    }

    /// Returns true when the client closed the entry locally.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Abandoned)
    }

    /// Returns true when the client asked to cancel the entry.
    #[must_use]
    pub const fn is_cancel_requested(self) -> bool {
        matches!(self, Self::CancelRequested)
    }
}

impl fmt::Display for LocalDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The scheduler state of one outbox entry.
///
/// Per-key FIFO orders every non-terminal entry. At most one entry per key is
/// in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SchedulerState {
    /// The entry waits for selection.
    Queued,
    /// The entry waits for an authority verdict.
    InFlight,
    /// The entry has a terminal local or authority outcome.
    Terminal,
}

impl SchedulerState {
    /// Every scheduler state.
    pub const ALL: [Self; 3] = [Self::Queued, Self::InFlight, Self::Terminal];

    /// Returns the canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InFlight => "in_flight",
            Self::Terminal => "terminal",
        }
    }

    /// Returns true when the state ends scheduler work for the entry.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

impl fmt::Display for SchedulerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One durable intent in the client outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    idempotency_key: IdempotencyKey,
    request_id: Option<RequestId>,
    body_digest: RequestDigest,
    target_shard: ShardId,
    base_versions: Vec<BaseVersion>,
    dependencies: Vec<IdempotencyKey>,
    disposition: LocalDisposition,
    scheduler_state: SchedulerState,
    arrival: u64,
    terminal_outcome: Option<ReceiptOutcome>,
}

impl OutboxEntry {
    /// Creates one outbox entry.
    #[must_use]
    pub fn new(
        idempotency_key: IdempotencyKey,
        body_digest: RequestDigest,
        target_shard: ShardId,
    ) -> Self {
        Self {
            idempotency_key,
            request_id: None,
            body_digest,
            target_shard,
            base_versions: Vec::new(),
            dependencies: Vec::new(),
            disposition: LocalDisposition::Queued,
            scheduler_state: SchedulerState::Queued,
            arrival: 0,
            terminal_outcome: None,
        }
    }

    /// Sets the request identity.
    ///
    /// Essay 24 uses this value as `origin_request_id`.
    #[must_use]
    pub fn with_request_id(mut self, request_id: RequestId) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// Adds one base version.
    #[must_use]
    pub fn with_base_version(mut self, key: Vec<u8>, base_version: KeyVersion) -> Self {
        self.base_versions.push(BaseVersion::new(key, base_version));
        self
    }

    /// Sets every base version.
    #[must_use]
    pub fn with_base_versions(mut self, base_versions: Vec<BaseVersion>) -> Self {
        self.base_versions = base_versions;
        self
    }

    /// Sets the explicit dependency edges.
    ///
    /// An edge names an earlier entry in the same shard.
    #[must_use]
    pub fn with_dependencies(mut self, dependencies: Vec<IdempotencyKey>) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> Option<RequestId> {
        self.request_id
    }

    /// Returns the request body digest.
    #[must_use]
    pub const fn body_digest(&self) -> RequestDigest {
        self.body_digest
    }

    /// Returns the target shard.
    #[must_use]
    pub const fn target_shard(&self) -> &ShardId {
        &self.target_shard
    }

    /// Returns every base version pair.
    #[must_use]
    pub fn base_versions(&self) -> &[BaseVersion] {
        &self.base_versions
    }

    /// Returns every dependency edge.
    #[must_use]
    pub fn dependencies(&self) -> &[IdempotencyKey] {
        &self.dependencies
    }

    /// Returns the local disposition.
    #[must_use]
    pub const fn disposition(&self) -> LocalDisposition {
        self.disposition
    }

    /// Returns the scheduler state.
    #[must_use]
    pub const fn scheduler_state(&self) -> SchedulerState {
        self.scheduler_state
    }

    /// Returns the arrival sequence.
    ///
    /// The outbox assigns this value at admission.
    #[must_use]
    pub const fn arrival(&self) -> u64 {
        self.arrival
    }

    /// Returns the terminal authority outcome.
    #[must_use]
    pub const fn terminal_outcome(&self) -> Option<ReceiptOutcome> {
        self.terminal_outcome
    }

    /// Returns true when the entry shares one base key with another entry.
    #[must_use]
    pub fn shares_key_with(&self, other: &Self) -> bool {
        self.base_versions.iter().any(|base| {
            other
                .base_versions
                .iter()
                .any(|candidate| candidate.key == base.key)
        })
    }

    /// Returns true when the entry mutates the given key.
    #[must_use]
    pub fn mutates_key(&self, key: &[u8]) -> bool {
        self.base_versions.iter().any(|base| base.key == key)
    }

    pub(crate) fn set_arrival(&mut self, arrival: u64) {
        self.arrival = arrival;
    }

    pub(crate) fn set_disposition(&mut self, disposition: LocalDisposition) {
        self.disposition = disposition;
    }

    pub(crate) fn set_scheduler_state(&mut self, state: SchedulerState) {
        self.scheduler_state = state;
    }

    pub(crate) fn set_terminal_outcome(&mut self, outcome: Option<ReceiptOutcome>) {
        self.terminal_outcome = outcome;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_cancel_never_names_a_receipt_state() {
        for disposition in [
            LocalDisposition::CancelRequested,
            LocalDisposition::Abandoned,
        ] {
            assert!(
                oxkv_contracts::receipt::ReceiptState::from_name(disposition.name()).is_none(),
                "the local disposition {disposition} is a receipt state"
            );
        }
    }

    #[test]
    fn an_entry_starts_queued_with_no_terminal_outcome() {
        let entry = OutboxEntry::new(
            IdempotencyKey::new(vec![1]),
            RequestDigest::from_bytes([2; 32]),
            ShardId::new(vec![3]),
        );
        assert_eq!(entry.disposition(), LocalDisposition::Queued);
        assert_eq!(entry.scheduler_state(), SchedulerState::Queued);
        assert_eq!(entry.terminal_outcome(), None);
        assert_eq!(entry.arrival(), 0);
    }
}
