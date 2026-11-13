//! The named errors of the snapshot contract.
//!
//! Each error maps to one code from the canonical registry. A caller can
//! compare [`SnapshotError::code`] without matching every field.

use oxkv_contracts::version::{PageId, ProvenanceField};

/// A named snapshot format error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The format major version is not supported.
    #[error("The format version {major}.{minor} is not supported at byte {offset}.")]
    UnsupportedVersion {
        /// The major version in the input.
        major: u16,
        /// The minor version in the input.
        minor: u16,
        /// The byte offset of the rejection.
        offset: u64,
    },
    /// A required feature bit is not known.
    #[error("The required feature bit {bit} is not supported at byte {offset}.")]
    MissingRequiredFeature {
        /// The lowest unknown feature bit.
        bit: u8,
        /// The byte offset of the rejection.
        offset: u64,
    },
    /// A page fails an integrity or shape check.
    #[error("The page {page_id} is corrupt at byte {offset}: {reason}.")]
    CorruptPage {
        /// The page identity.
        page_id: PageId,
        /// The byte offset of the page.
        offset: u64,
        /// A short reason for the rejection.
        reason: &'static str,
    },
    /// A page key is outside the shard prefix bounds.
    #[error("The page {page_id} is outside the shard bounds at byte {offset}.")]
    PageOutOfBounds {
        /// The page identity.
        page_id: PageId,
        /// The byte offset of the page.
        offset: u64,
    },
    /// The input ends before a required byte range.
    #[error("The snapshot is truncated at byte {offset}.")]
    TruncatedSnapshot {
        /// The byte offset where the input ends.
        offset: u64,
    },
    /// A required provenance field is absent.
    #[error("The snapshot is missing the required provenance field {field}.")]
    MissingProvenance {
        /// The missing field.
        field: ProvenanceField,
    },
    /// A length field is over its limit.
    #[error("The page {page_id} is too large at byte {offset}.")]
    PageTooLarge {
        /// The page identity. It is all zero before page creation.
        page_id: PageId,
        /// The byte offset of the page or field.
        offset: u64,
    },
    /// The content seal does not cover the content digest.
    #[error("The content seal is stale at byte {offset}.")]
    StaleSeal {
        /// The byte offset of the seal.
        offset: u64,
    },
    /// The text header is not a supported header.
    #[error("The snapshot header is malformed at byte {offset}: {reason}.")]
    MalformedHeader {
        /// The byte offset of the malformed field.
        offset: u64,
        /// A short reason for the rejection.
        reason: &'static str,
    },
    /// A scope row uses a forbidden system namespace.
    #[error("The scope snapshot cannot include the system namespace {id}.")]
    ForbiddenNamespace {
        /// The forbidden system identifier.
        id: u8,
    },
    /// A user row path received a system key.
    #[error("The user row path cannot include the system namespace {id}.")]
    SystemKeyNotAllowed {
        /// The system identifier.
        id: u8,
    },
    /// A projection path received a key that is not a projection fact.
    #[error("The key {key:?} is not a projection fact.")]
    NotProjectionFact {
        /// The rejected key.
        key: Vec<u8>,
    },
    /// The scope does not admit a record.
    #[error("The scope does not admit the record {key:?}.")]
    OutOfScopeRecord {
        /// The rejected key.
        key: Vec<u8>,
    },
    /// A builder input has two records with one key.
    #[error("The record key is duplicated at byte {offset}.")]
    DuplicateKey {
        /// The byte offset of the duplicate record.
        offset: u64,
    },
}

impl SnapshotError {
    /// Returns the canonical error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion { .. } | Self::MalformedHeader { .. } => {
                "format.unsupported_version"
            }
            Self::MissingRequiredFeature { .. } => "format.missing_required_feature",
            Self::CorruptPage { .. }
            | Self::DuplicateKey { .. }
            | Self::ForbiddenNamespace { .. } => "format.corrupt_page",
            Self::SystemKeyNotAllowed { .. }
            | Self::NotProjectionFact { .. }
            | Self::OutOfScopeRecord { .. } => "internal.invariant_violation",
            Self::PageOutOfBounds { .. } => "format.page_out_of_bounds",
            Self::TruncatedSnapshot { .. } => "format.truncated_snapshot",
            Self::MissingProvenance { .. } => "format.missing_provenance",
            Self::PageTooLarge { .. } => "format.page_too_large",
            Self::StaleSeal { .. } => "replication.stale_epoch",
        }
    }

    /// Returns the byte offset of the rejection.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        match self {
            Self::UnsupportedVersion { offset, .. }
            | Self::MissingRequiredFeature { offset, .. }
            | Self::CorruptPage { offset, .. }
            | Self::PageOutOfBounds { offset, .. }
            | Self::TruncatedSnapshot { offset }
            | Self::PageTooLarge { offset, .. }
            | Self::StaleSeal { offset }
            | Self::MalformedHeader { offset, .. }
            | Self::DuplicateKey { offset } => *offset,
            Self::MissingProvenance { .. }
            | Self::ForbiddenNamespace { .. }
            | Self::SystemKeyNotAllowed { .. }
            | Self::NotProjectionFact { .. }
            | Self::OutOfScopeRecord { .. } => 0,
        }
    }
}
