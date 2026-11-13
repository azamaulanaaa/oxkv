//! Receipt states and the receipt envelope.
//!
//! A receipt has four states. [`ReceiptState::Queued`] is live. The other
//! three states are terminal. A terminal receipt cannot change state.
//!
//! Only the authority writes a receipt verdict about data. A client cancel is
//! a local outbox disposition. It is not a receipt state.

use core::fmt;
use core::str::FromStr;

use crate::error::{ErrorRegistryError, lookup_receipt_reason};
use crate::version::{CommitVersion, Hash32, KeyVersion, ManifestDigest, PolicyVersion, ShardId};

/// The four receipt states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReceiptState {
    /// The write is live. It is not terminal.
    Queued,
    /// The write is durable. The state is terminal.
    Committed,
    /// The authority refused the write. The state is terminal.
    Rejected,
    /// The base version did not match. The state is terminal.
    Conflict,
}

impl ReceiptState {
    /// Every receipt state.
    pub const ALL: [Self; 4] = [
        Self::Queued,
        Self::Committed,
        Self::Rejected,
        Self::Conflict,
    ];

    /// Returns the canonical state name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Committed => "Committed",
            Self::Rejected => "Rejected",
            Self::Conflict => "Conflict",
        }
    }

    /// Returns the stable wire tag.
    ///
    /// The tag does not depend on the Rust enum discriminant.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        match self {
            Self::Queued => 1,
            Self::Committed => 2,
            Self::Rejected => 3,
            Self::Conflict => 4,
        }
    }

    /// Reads a state from its stable wire tag.
    #[must_use]
    pub const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Queued),
            2 => Some(Self::Committed),
            3 => Some(Self::Rejected),
            4 => Some(Self::Conflict),
            _ => None,
        }
    }

    /// Reads a state from its canonical name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|state| state.name() == name)
    }

    /// Returns true when the state is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Queued => false,
            Self::Committed | Self::Rejected | Self::Conflict => true,
        }
    }
}

impl fmt::Display for ReceiptState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// An error from receipt state name parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("The receipt state name is unknown.")]
pub struct ReceiptStateParseError;

impl FromStr for ReceiptState {
    type Err = ReceiptStateParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::from_name(text).ok_or(ReceiptStateParseError)
    }
}

/// An error from a receipt state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptTransitionError {
    /// The receipt is already terminal.
    #[error("A terminal receipt cannot change state.")]
    Terminal {
        /// The terminal state.
        state: ReceiptState,
    },
    /// The target state is not terminal.
    #[error("A receipt can only close into a terminal state.")]
    NotTerminal {
        /// The target state.
        state: ReceiptState,
    },
    /// The reason code is not a registered receipt reason.
    #[error("The receipt reason is not registered: {code}.")]
    UnknownReason {
        /// The unregistered reason code.
        code: &'static str,
    },
}

impl ReceiptTransitionError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Terminal { .. } | Self::NotTerminal { .. } | Self::UnknownReason { .. } => {
                "receipt.state_invalid"
            }
        }
    }
}

/// The terminal outcome that closes one receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiptOutcome {
    /// The write is durable at the given shard commit version.
    Committed {
        /// The resulting shard commit version.
        commit_version: CommitVersion,
    },
    /// The authority refused the write for the given reason code.
    Rejected {
        /// The canonical reason code.
        reason: &'static str,
    },
    /// The base version did not match the current key version.
    Conflict {
        /// The current key version.
        current_key_version: KeyVersion,
        /// The canonical reason code.
        reason: &'static str,
    },
}

impl ReceiptOutcome {
    /// Returns the terminal state of the outcome.
    #[must_use]
    pub const fn state(self) -> ReceiptState {
        match self {
            Self::Committed { .. } => ReceiptState::Committed,
            Self::Rejected { .. } => ReceiptState::Rejected,
            Self::Conflict { .. } => ReceiptState::Conflict,
        }
    }
}

/// A receipt identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReceiptId(Hash32);

impl ReceiptId {
    /// Creates a receipt identity from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for ReceiptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ReceiptId")
            .field(&self.0.to_hex())
            .finish()
    }
}

/// A request identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(Hash32);

impl RequestId {
    /// Creates a request identity from raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Hash32::from_bytes(bytes))
    }

    /// Returns the raw identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RequestId")
            .field(&self.0.to_hex())
            .finish()
    }
}

/// A client idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IdempotencyKey(Vec<u8>);

impl IdempotencyKey {
    /// Creates an idempotency key.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for IdempotencyKey {
    fn from(bytes: Vec<u8>) -> Self {
        Self::new(bytes)
    }
}

/// One position in a per-shard audit chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AuditSequence(u64);

impl AuditSequence {
    /// Creates an audit sequence value.
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

impl fmt::Display for AuditSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// A reference to the audit record that justified a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuditReference {
    /// The shard that owns the audit chain.
    pub shard_id: ShardId,
    /// The sequence in the per-shard chain.
    pub audit_sequence: AuditSequence,
    /// The hash of the audit record.
    pub record_hash: Hash32,
}

impl AuditReference {
    /// Creates an audit reference.
    #[must_use]
    pub const fn new(
        shard_id: ShardId,
        audit_sequence: AuditSequence,
        record_hash: Hash32,
    ) -> Self {
        Self {
            shard_id,
            audit_sequence,
            record_hash,
        }
    }
}

/// The durable envelope of one receipt.
///
/// The envelope carries the audit reference, the pinned policy version, and
/// the manifest digest. A `Committed` envelope carries the shard commit
/// version. A `Conflict` envelope carries the current key version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptEnvelope {
    receipt_id: ReceiptId,
    request_id: RequestId,
    state: ReceiptState,
    idempotency_key: IdempotencyKey,
    audit: AuditReference,
    policy_version: PolicyVersion,
    manifest_digest: ManifestDigest,
    commit_version: Option<CommitVersion>,
    current_key_version: Option<KeyVersion>,
    reason_code: Option<&'static str>,
}

impl ReceiptEnvelope {
    /// Creates a `Queued` receipt.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorRegistryError`] when the reason code is absent or maps
    /// to no receipt.
    pub fn queued(
        receipt_id: ReceiptId,
        request_id: RequestId,
        idempotency_key: IdempotencyKey,
        audit: AuditReference,
        policy_version: PolicyVersion,
        manifest_digest: ManifestDigest,
        reason_code: &'static str,
    ) -> Result<Self, ErrorRegistryError> {
        lookup_receipt_reason(reason_code)?;
        Ok(Self {
            receipt_id,
            request_id,
            state: ReceiptState::Queued,
            idempotency_key,
            audit,
            policy_version,
            manifest_digest,
            commit_version: None,
            current_key_version: None,
            reason_code: Some(reason_code),
        })
    }

    /// Closes a queued receipt with one terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiptTransitionError::Terminal`] when the receipt is
    /// already terminal. Returns [`ReceiptTransitionError::UnknownReason`] when
    /// the outcome reason is not a registered receipt reason.
    pub fn close(&mut self, outcome: ReceiptOutcome) -> Result<(), ReceiptTransitionError> {
        if self.state.is_terminal() {
            return Err(ReceiptTransitionError::Terminal { state: self.state });
        }
        match outcome {
            ReceiptOutcome::Committed { commit_version } => {
                self.state = ReceiptState::Committed;
                self.commit_version = Some(commit_version);
                self.current_key_version = None;
                self.reason_code = None;
            }
            ReceiptOutcome::Rejected { reason } => {
                lookup_receipt_reason(reason)
                    .map_err(|_error| ReceiptTransitionError::UnknownReason { code: reason })?;
                self.state = ReceiptState::Rejected;
                self.commit_version = None;
                self.current_key_version = None;
                self.reason_code = Some(reason);
            }
            ReceiptOutcome::Conflict {
                current_key_version,
                reason,
            } => {
                lookup_receipt_reason(reason)
                    .map_err(|_error| ReceiptTransitionError::UnknownReason { code: reason })?;
                self.state = ReceiptState::Conflict;
                self.commit_version = None;
                self.current_key_version = Some(current_key_version);
                self.reason_code = Some(reason);
            }
        }
        Ok(())
    }

    /// Returns the receipt state.
    #[must_use]
    pub const fn state(&self) -> ReceiptState {
        self.state
    }

    /// Returns true when the receipt is terminal.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// Returns the receipt identity.
    #[must_use]
    pub const fn receipt_id(&self) -> ReceiptId {
        self.receipt_id
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Returns the idempotency key.
    #[must_use]
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the audit reference.
    #[must_use]
    pub const fn audit(&self) -> &AuditReference {
        &self.audit
    }

    /// Returns the pinned policy version.
    #[must_use]
    pub const fn policy_version(&self) -> PolicyVersion {
        self.policy_version
    }

    /// Returns the manifest digest.
    #[must_use]
    pub const fn manifest_digest(&self) -> ManifestDigest {
        self.manifest_digest
    }

    /// Returns the shard commit version of a `Committed` receipt.
    #[must_use]
    pub const fn commit_version(&self) -> Option<CommitVersion> {
        self.commit_version
    }

    /// Returns the current key version of a `Conflict` receipt.
    #[must_use]
    pub const fn current_key_version(&self) -> Option<KeyVersion> {
        self.current_key_version
    }

    /// Returns the reason code of a live or rejected receipt.
    #[must_use]
    pub const fn reason_code(&self) -> Option<&'static str> {
        self.reason_code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_receipt() -> ReceiptEnvelope {
        ReceiptEnvelope::queued(
            ReceiptId::from_bytes([1; 32]),
            RequestId::from_bytes([2; 32]),
            IdempotencyKey::new(vec![9, 9]),
            AuditReference::new(
                ShardId::new(vec![1]),
                AuditSequence::new(7),
                Hash32::from_bytes([3; 32]),
            ),
            PolicyVersion::from_bytes([4; 32]),
            ManifestDigest::from_bytes([5; 32]),
            "projection.lagging",
        )
        .expect("the reason code is registered")
    }

    #[test]
    fn there_are_exactly_four_receipt_states() {
        assert_eq!(ReceiptState::ALL.len(), 4);
        assert_eq!(
            ReceiptState::ALL,
            [
                ReceiptState::Queued,
                ReceiptState::Committed,
                ReceiptState::Rejected,
                ReceiptState::Conflict,
            ]
        );
    }

    #[test]
    fn a_fifth_state_does_not_exist() {
        assert!(ReceiptState::from_name("Canceled").is_none());
        assert!(ReceiptState::from_name("Abandoned").is_none());
        assert!(ReceiptState::from_wire_tag(0).is_none());
        assert!(ReceiptState::from_wire_tag(5).is_none());
        for state in ReceiptState::ALL {
            assert_eq!(ReceiptState::from_wire_tag(state.wire_tag()), Some(state));
            assert_eq!(ReceiptState::from_name(state.name()), Some(state));
        }
    }

    #[test]
    fn only_queued_is_live() {
        assert!(!ReceiptState::Queued.is_terminal());
        assert!(ReceiptState::Committed.is_terminal());
        assert!(ReceiptState::Rejected.is_terminal());
        assert!(ReceiptState::Conflict.is_terminal());
    }

    #[test]
    fn a_terminal_receipt_cannot_change_state() {
        let mut receipt = sample_receipt();
        receipt
            .close(ReceiptOutcome::Committed {
                commit_version: CommitVersion::new(11),
            })
            .expect("a queued receipt closes once");
        assert_eq!(receipt.state(), ReceiptState::Committed);
        assert_eq!(receipt.commit_version(), Some(CommitVersion::new(11)));

        let result = receipt.close(ReceiptOutcome::Rejected {
            reason: "write.queue_timeout",
        });
        assert_eq!(
            result,
            Err(ReceiptTransitionError::Terminal {
                state: ReceiptState::Committed
            })
        );
        assert_eq!(receipt.state(), ReceiptState::Committed);
        assert_eq!(receipt.commit_version(), Some(CommitVersion::new(11)));
        assert_eq!(receipt.reason_code(), None);
    }

    #[test]
    fn a_conflict_names_the_current_key_version() {
        let mut receipt = sample_receipt();
        receipt
            .close(ReceiptOutcome::Conflict {
                current_key_version: KeyVersion::new(8),
                reason: "write.version_conflict",
            })
            .expect("a queued receipt closes once");
        assert_eq!(receipt.state(), ReceiptState::Conflict);
        assert_eq!(receipt.current_key_version(), Some(KeyVersion::new(8)));
        assert_eq!(receipt.reason_code(), Some("write.version_conflict"));
        assert_eq!(receipt.commit_version(), None);
    }

    #[test]
    fn a_rejected_receipt_names_its_reason() {
        let mut receipt = sample_receipt();
        receipt
            .close(ReceiptOutcome::Rejected {
                reason: "write.queue_timeout",
            })
            .expect("a queued receipt closes once");
        assert_eq!(receipt.state(), ReceiptState::Rejected);
        assert_eq!(receipt.reason_code(), Some("write.queue_timeout"));
        assert_eq!(receipt.commit_version(), None);
    }

    #[test]
    fn a_queued_receipt_reason_must_be_registered() {
        assert!(
            ReceiptEnvelope::queued(
                ReceiptId::from_bytes([1; 32]),
                RequestId::from_bytes([2; 32]),
                IdempotencyKey::new(vec![9, 9]),
                AuditReference::new(
                    ShardId::new(vec![1]),
                    AuditSequence::new(7),
                    Hash32::from_bytes([3; 32]),
                ),
                PolicyVersion::from_bytes([4; 32]),
                ManifestDigest::from_bytes([5; 32]),
                "projection.lagging",
            )
            .is_ok()
        );
        assert_eq!(
            ReceiptEnvelope::queued(
                ReceiptId::from_bytes([1; 32]),
                RequestId::from_bytes([2; 32]),
                IdempotencyKey::new(vec![9, 9]),
                AuditReference::new(
                    ShardId::new(vec![1]),
                    AuditSequence::new(7),
                    Hash32::from_bytes([3; 32]),
                ),
                PolicyVersion::from_bytes([4; 32]),
                ManifestDigest::from_bytes([5; 32]),
                "write.not_registered",
            ),
            Err(ErrorRegistryError::Unknown {
                code: "write.not_registered".to_owned()
            })
        );
        assert_eq!(
            ReceiptEnvelope::queued(
                ReceiptId::from_bytes([1; 32]),
                RequestId::from_bytes([2; 32]),
                IdempotencyKey::new(vec![9, 9]),
                AuditReference::new(
                    ShardId::new(vec![1]),
                    AuditSequence::new(7),
                    Hash32::from_bytes([3; 32]),
                ),
                PolicyVersion::from_bytes([4; 32]),
                ManifestDigest::from_bytes([5; 32]),
                "audit.unavailable",
            ),
            Err(ErrorRegistryError::NoReceipt {
                code: "audit.unavailable".to_owned()
            })
        );
    }

    #[test]
    fn a_terminal_receipt_reason_must_be_registered() {
        let mut receipt = sample_receipt();
        assert_eq!(
            receipt.close(ReceiptOutcome::Rejected {
                reason: "write.not_registered",
            }),
            Err(ReceiptTransitionError::UnknownReason {
                code: "write.not_registered"
            })
        );
        assert_eq!(receipt.state(), ReceiptState::Queued);
        assert_eq!(receipt.reason_code(), Some("projection.lagging"));

        let mut receipt = sample_receipt();
        assert_eq!(
            receipt.close(ReceiptOutcome::Conflict {
                current_key_version: KeyVersion::new(8),
                reason: "audit.unavailable",
            }),
            Err(ReceiptTransitionError::UnknownReason {
                code: "audit.unavailable"
            })
        );
        assert_eq!(receipt.state(), ReceiptState::Queued);
    }

    #[test]
    fn the_envelope_names_every_required_field() {
        let receipt = sample_receipt();
        assert_eq!(receipt.receipt_id(), ReceiptId::from_bytes([1; 32]));
        assert_eq!(receipt.request_id(), RequestId::from_bytes([2; 32]));
        assert_eq!(receipt.idempotency_key().as_bytes(), &[9, 9]);
        assert_eq!(receipt.audit().audit_sequence, AuditSequence::new(7));
        assert_eq!(receipt.policy_version(), PolicyVersion::from_bytes([4; 32]));
        assert_eq!(
            receipt.manifest_digest(),
            ManifestDigest::from_bytes([5; 32])
        );
    }
}
