//! Named projection errors.
//!
//! Every error maps to one row of the canonical error registry in
//! `oxkv-contracts`. The projection never invents a code outside that registry.

use oxkv_contracts::error::lookup;
use oxkv_contracts::receipt::ReceiptState;
use oxkv_contracts::version::{FactSourceId, FactVersion};

use crate::fact::FactId;

/// The projection client-write error code.
pub const PROJECTION_CLIENT_WRITE_REJECTED: &str = "projection.client_write_rejected";
/// The projection epoch error code.
pub const PROJECTION_EPOCH_INVALID: &str = "projection.epoch_invalid";
/// The projection fact-set mismatch error code.
pub const PROJECTION_FACT_SET_MISMATCH: &str = "projection.fact_set_mismatch";
/// The projection fact-type mismatch error code.
pub const PROJECTION_FACT_TYPE_MISMATCH: &str = "projection.fact_type_mismatch";
/// The projection feed sequence error code.
pub const PROJECTION_FEED_SEQUENCE_INVALID: &str = "projection.feed_sequence_invalid";
/// The projection watermark regression error code.
pub const PROJECTION_WATERMARK_REGRESSION: &str = "projection.watermark_regression";
/// The projection checksum error code.
pub const PROJECTION_CHECKSUM_FAILED: &str = "projection.checksum_failed";
/// The projection fan-in error code.
pub const PROJECTION_FAN_IN_EXCEEDED: &str = "projection.fan_in_exceeded";
/// The projection read-set error code.
pub const PROJECTION_READ_SET_EXCEEDED: &str = "projection.read_set_exceeded";
/// The projection lagging reason code.
pub const PROJECTION_LAGGING: &str = "projection.lagging";
/// The queue deadline reason code.
pub const WRITE_QUEUE_TIMEOUT: &str = "write.queue_timeout";
/// The strict authorization reason code.
pub const AUTHORIZATION_STALE_FACTS: &str = "authorization.stale_facts";

/// One projection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// A client addressed a write to the projection namespace.
    #[error("A client cannot write a projection.")]
    ClientWriteRejected,
    /// The projection epoch is not valid for this operation.
    #[error("The projection epoch is not valid for this operation.")]
    EpochInvalid,
    /// A fact version or source watermark moved backward.
    #[error(
        "The fact source {} moved backward from {current} to {incoming}.",
        fact_source.as_str()
    )]
    WatermarkRegression {
        /// The fact source.
        fact_source: FactSourceId,
        /// The current watermark or fact version.
        current: FactVersion,
        /// The incoming watermark or fact version.
        incoming: FactVersion,
    },
    /// A snapshot or diff checksum did not match its bytes.
    #[error("The projection artifact checksum does not match its bytes.")]
    ChecksumFailed,
    /// A read set names more fact sources than the cap permits.
    #[error("The read set names {sources} sources. The fan-in cap is {cap}.")]
    FanInExceeded {
        /// The observed source count.
        sources: u32,
        /// The declared cap.
        cap: u32,
    },
    /// A read set names more facts than the cap permits.
    #[error("The read set names {facts} facts. The read-set cap is {cap}.")]
    ReadSetExceeded {
        /// The observed fact count.
        facts: u32,
        /// The declared cap.
        cap: u32,
    },
    /// A read names a fact that the fact set does not declare.
    #[error("The fact {fact_id} is not declared.")]
    UnknownFact {
        /// The undeclared fact identity.
        fact_id: FactId,
    },
    /// A fact set declares one identity more than one time.
    #[error("The fact {fact_id} is declared more than one time.")]
    DuplicateFact {
        /// The duplicate fact identity.
        fact_id: FactId,
    },
    /// A declared fact has no projected value.
    #[error("The fact {fact_id} is not present in the projection.")]
    FactUnavailable {
        /// The missing fact identity.
        fact_id: FactId,
    },
    /// A snapshot carries a different fact set than the projection.
    #[error("The snapshot fact set does not match the projection fact set.")]
    FactSetMismatch,
    /// A projected value does not match its declared canonical type.
    #[error("The fact {fact_id} does not match its declared canonical type.")]
    FactTypeMismatch {
        /// The mismatched fact identity.
        fact_id: FactId,
    },
    /// A diff sequence is not contiguous.
    #[error("The diff sequence {found} is not the expected sequence {expected}.")]
    FeedSequenceInvalid {
        /// The expected sequence.
        expected: u64,
        /// The observed sequence.
        found: u64,
    },
    /// An epoch counter overflowed.
    #[error("The projection epoch counter overflowed.")]
    EpochOverflow,
}

impl ProjectionError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ClientWriteRejected => PROJECTION_CLIENT_WRITE_REJECTED,
            Self::EpochInvalid => PROJECTION_EPOCH_INVALID,
            Self::FactSetMismatch => PROJECTION_FACT_SET_MISMATCH,
            Self::FactTypeMismatch { .. } => PROJECTION_FACT_TYPE_MISMATCH,
            Self::FeedSequenceInvalid { .. } => PROJECTION_FEED_SEQUENCE_INVALID,
            Self::WatermarkRegression { .. } => PROJECTION_WATERMARK_REGRESSION,
            Self::ChecksumFailed => PROJECTION_CHECKSUM_FAILED,
            Self::FanInExceeded { .. } => PROJECTION_FAN_IN_EXCEEDED,
            Self::ReadSetExceeded { .. } => PROJECTION_READ_SET_EXCEEDED,
            Self::UnknownFact { .. } => "projection.unknown_fact",
            Self::DuplicateFact { .. } => "projection.duplicate_fact",
            Self::FactUnavailable { .. } => "projection.fact_unavailable",
            Self::EpochOverflow => "projection.epoch_overflow",
        }
    }

    /// Returns the receipt state that the registry assigns to the code.
    ///
    /// A code with no receipt returns `None`.
    #[must_use]
    pub fn receipt_state(&self) -> Option<ReceiptState> {
        lookup(self.code()).and_then(|spec| spec.receipt)
    }
}
